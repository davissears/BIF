use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use bif::{
    application::{
        Actor, ActorKind, AuthorizationRequest, CaptureIdentity, CaptureInput, CaptureRequest,
        CaptureStore, Command, Execution, HumanAuthorization, ItemListFilters, ItemProjection,
        ItemProjectionKind, ItemSummary, MutationIdentity, MutationRequest, MutationStore,
        ObservedExecution, read_semantics::EffectiveItemFilters,
    },
    config::Config,
    domain::{
        AssigneeId, Item, ItemContent, ItemId, ItemMutation, LifecycleMutation, NamedView,
        Priority, ProjectId, Provenance, RequesterId, Revision, Status, Timestamp, Triage,
        TriageField,
    },
    read_session::{ReadRequest, ReadSession},
    storage::{self, CaptureRepository, MutationRepository, maintenance},
    v2_response::{ResponseBudget, write_get},
};
use serde_json::{Value, json};

// These are contract models, not evidence that the deferred sync runtime exists.
fn document(name: &str) -> Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("docs/fixtures")
        .join(name);
    serde_json::from_str(
        &fs::read_to_string(&path).unwrap_or_else(|error| {
            panic!("required contract fixture {}: {error}", path.display())
        }),
    )
    .expect("valid JSON")
}

fn fixture() -> Value {
    document("bif-v2-sync-contract.json")
}

fn validator() -> jsonschema::Validator {
    jsonschema::validator_for(&document("bif-v2-sync-contract.schema.json"))
        .expect("valid strict schema")
}

fn array(value: &Value) -> &[Value] {
    value.as_array().expect("array")
}

fn number(value: &Value) -> u64 {
    value.as_u64().expect("unsigned integer")
}

fn text(value: &Value) -> &str {
    value.as_str().expect("string")
}

fn case<'a>(cases: &'a Value, id: &str) -> &'a Value {
    array(cases)
        .iter()
        .find(|case| case["id"] == id)
        .unwrap_or_else(|| panic!("missing contract case {id}"))
}

// F-P01 checks agreement among declared examples, not a new consumer state machine.
#[test]
fn activation_examples_preserve_history_and_independent_client_state() {
    let fixture = fixture();
    let activation = &fixture["activation"];
    assert_eq!(activation["initial_sync_enabled"], false);
    assert_eq!(activation["service_contract"]["mode"], "authorized_offline");
    assert_eq!(
        activation["service_contract"]["preserves"],
        json!([
            "generation",
            "floor",
            "high_water",
            "bootstrap_required",
            "client_state"
        ])
    );
    for id in ["enable", "retry", "reactivate", "committed_without_ack"] {
        let attempt = case(&activation["attempts"], id);
        assert_eq!(attempt["after"]["sync_enabled"], true, "{id}");
        for field in ["generation", "floor", "high_water", "bootstrap_required"] {
            assert_eq!(
                attempt["before"][field], attempt["after"][field],
                "{id}: {field}"
            );
        }
        assert_eq!(attempt["client_state_changed"], false);
        assert_eq!(
            attempt["expected_generation"],
            attempt["before"]["generation"]
        );
        assert!(attempt["rejection"].is_null());
        for prerequisite in attempt["prerequisites"].as_object().unwrap().values() {
            assert_eq!(prerequisite, true, "{id}");
        }
    }
    for id in [
        "unauthorized",
        "not_offline",
        "not_reaped",
        "restart_allowed",
        "unsupported_writers",
        "wrong_generation",
        "crash_before_commit",
    ] {
        let attempt = case(&activation["attempts"], id);
        assert_eq!(attempt["before"], attempt["after"], "{id}");
        assert_eq!(attempt["client_state_changed"], false);
    }
    for (id, prerequisite, rejection) in [
        ("unauthorized", "authorized", "unauthorized"),
        ("not_offline", "offline", "offline_required"),
        (
            "not_reaped",
            "source_processes_reaped",
            "source_processes_running",
        ),
        (
            "restart_allowed",
            "restart_excluded",
            "restart_not_excluded",
        ),
        (
            "unsupported_writers",
            "supported_writers_journal",
            "unsupported_writers",
        ),
    ] {
        let attempt = case(&activation["attempts"], id);
        assert_eq!(attempt["prerequisites"][prerequisite], false);
        assert_eq!(attempt["rejection"], rejection);
    }
    let wrong_generation = case(&activation["attempts"], "wrong_generation");
    assert_ne!(
        wrong_generation["expected_generation"],
        wrong_generation["before"]["generation"]
    );
    assert_eq!(wrong_generation["rejection"], "wrong_generation");
    let restore = case(&activation["attempts"], "restore");
    assert_ne!(
        restore["before"]["generation"],
        restore["after"]["generation"]
    );
    assert_eq!(restore["after"]["sync_enabled"], false);
    assert_eq!(restore["after"]["bootstrap_required"], true);
    let reactivation = case(&activation["attempts"], "reactivate");
    assert_eq!(restore["after"], reactivation["before"]);
    let restart = case(&activation["attempts"], "restart");
    assert_eq!(restart["before"], restart["after"]);
    let disabled_restart = case(&activation["attempts"], "disabled_restart");
    assert_eq!(disabled_restart["before"], disabled_restart["after"]);
    assert_eq!(disabled_restart["after"]["sync_enabled"], false);

    let mirror = &fixture["mirror"];
    let complete = case(&mirror["statuses"], "client-a-complete");
    let absent = case(&mirror["statuses"], "client-b-absent");
    assert_ne!(complete["client_id"], absent["client_id"]);
    assert_ne!(complete["mirror_path"], absent["mirror_path"]);
    assert_eq!(complete["status"]["published_baseline"]["complete"], true);
    assert!(absent["status"]["published_baseline"].is_null());
    assert_eq!(
        activation["service_contract"]["server_client_registry"],
        false
    );
    assert_eq!(
        activation["service_contract"]["client_finish_clears_bootstrap_required"],
        false
    );
}

#[test]
fn status_examples_distinguish_empty_missing_rebuilding_and_unusable_baselines() {
    let fixture = fixture();
    let mirror = &fixture["mirror"];
    let status = |id| &case(&mirror["statuses"], id)["status"];
    let empty = status("complete-empty");
    assert_eq!(empty["published_baseline"]["complete"], true);
    assert_eq!(empty["published_baseline"]["item_count"], 0);
    assert!(!empty["published_baseline"]["checkpoint"].is_null());
    assert!(status("client-b-absent")["published_baseline"].is_null());
    let rebuilding = status("rebuilding");
    assert_eq!(
        rebuilding["published_baseline"],
        status("client-a-complete")["published_baseline"]
    );
    assert_eq!(rebuilding["rebuild_phase"], "scanning");
    assert_eq!(
        status("incompatible")["published_baseline"]["usability"],
        "incompatible_history"
    );
    assert_eq!(
        status("corrupt")["published_baseline"]["usability"],
        "corrupt_not_servable"
    );
    for entry in array(&mirror["statuses"]) {
        assert_eq!(
            entry["status"]["observation_scope"],
            "last_observed_not_current_now"
        );
    }
    for transition in array(&mirror["transitions"]) {
        let before = case(&mirror["statuses"], text(&transition["before"]));
        let after = case(&mirror["statuses"], text(&transition["after"]));
        assert_eq!(before["client_id"], after["client_id"]);
        assert_eq!(before["mirror_path"], after["mirror_path"]);
    }
    assert_eq!(
        status("catching-up")["published_baseline"],
        rebuilding["published_baseline"]
    );
    assert_eq!(
        status("catching-up")["staging_identity"],
        rebuilding["staging_identity"]
    );
    assert_eq!(
        status("published")["published_baseline"]["checkpoint"],
        fixture["bootstrap"]["catch_up_checkpoint"]
    );
    assert_eq!(
        status("published")["last_successful_catch_up"]["high_water"],
        fixture["bootstrap"]["catch_up_high_water"]
    );
    let transitions = &mirror["transitions"];
    for (id, action) in [
        ("transient", "retry"),
        ("expired", "rebuild"),
        ("generation", "rebuild"),
        ("baseline_loss", "rebuild"),
        ("corruption", "repair_local"),
        ("unfit", "resolve_record_or_budget"),
        ("disabled", "activate_source"),
    ] {
        let transition = case(transitions, id);
        let before = status(text(&transition["before"]));
        let after = status(text(&transition["after"]));
        assert_eq!(after["last_failure"]["recovery_action"], action);
        assert_eq!(transition["checkpoint_advanced"], false);
        assert_eq!(
            after["last_successful_catch_up"],
            before["last_successful_catch_up"]
        );
    }
    for callback in array(&mirror["callbacks"]) {
        assert_eq!(callback["expected"], "discarded");
        assert_eq!(callback["persisted_before"], callback["persisted_after"]);
        assert_ne!(callback["captured"], callback["persisted_before"]);
        assert_eq!(
            status(text(&callback["status_before"])),
            status(text(&callback["status_after"]))
        );
        let persisted_status = status(text(&callback["status_before"]));
        assert_eq!(
            callback["persisted_before"]["baseline_identity"],
            persisted_status["published_baseline"]["identity"]
        );
        assert_eq!(
            callback["persisted_before"]["staging_identity"],
            persisted_status["staging_identity"]
        );
        if text(&callback["id"]).starts_with("duplicate_no_change") {
            assert_eq!(
                callback["captured"]["checkpoint"],
                callback["persisted_before"]["checkpoint"]
            );
            assert_ne!(
                callback["captured"]["attempt_id"],
                callback["persisted_before"]["attempt_id"]
            );
        }
    }
    for kind in ["success", "failure"] {
        assert!(
            array(&mirror["callbacks"])
                .iter()
                .any(|callback| callback["kind"] == kind)
        );
    }
}

#[test]
fn wrong_store_error_has_a_schema_valid_incompatible_mirror_recovery() {
    let fixture = fixture();
    let transition = case(&fixture["mirror"]["transitions"], "store");
    let statuses = &fixture["mirror"]["statuses"];
    let before = &case(statuses, text(&transition["before"]))["status"];
    let after = &case(statuses, text(&transition["after"]))["status"];
    let error = &case(&fixture["service_requests"], "wrong_store_disabled")["response"]["error"];
    assert_eq!(error["details"]["reason"], "wrong_store");
    assert_eq!(after["last_failure"]["code"], error["details"]["reason"]);
    assert_eq!(after["last_failure"]["recovery_action"], "rebuild");
    assert_eq!(after["source_compatibility"], "incompatible");
    assert_eq!(after["rebuild_phase"], "blocked");
    assert!(after["staging_identity"].is_null());
    assert_eq!(
        after["published_baseline"]["usability"],
        "incompatible_history"
    );
    assert_eq!(transition["checkpoint_advanced"], false);
    for field in ["identity", "complete", "checkpoint", "item_count"] {
        assert_eq!(
            before["published_baseline"][field],
            after["published_baseline"][field]
        );
    }
    assert_eq!(
        before["last_successful_catch_up"],
        after["last_successful_catch_up"]
    );
    let validator = validator();
    assert!(
        validator.is_valid(&fixture),
        "{:?}",
        validator.iter_errors(&fixture).collect::<Vec<_>>()
    );
}

#[test]
fn disabled_requests_and_identity_precedence_are_explicit_contract_examples() {
    let fixture = fixture();
    let requests = &fixture["service_requests"];
    for id in ["disabled_initial_scan", "disabled_scan", "disabled_poll"] {
        let request = case(requests, id);
        assert_eq!(request["response"]["error"]["code"], "sync_disabled");
        assert_eq!(request["checkpoint_advanced"], false);
    }
    assert!(case(requests, "disabled_initial_scan")["cursor"].is_null());
    assert_eq!(
        case(requests, "unauthorized_wrong_identity_disabled")["response"]["error"]["code"],
        "unauthorized"
    );
    for (id, reason) in [
        ("wrong_generation_disabled", "wrong_generation"),
        ("wrong_store_disabled", "wrong_store"),
    ] {
        let error = &case(requests, id)["response"]["error"];
        assert_eq!(error["code"], "invalid_cursor");
        assert_eq!(error["details"]["reason"], reason);
    }
}

#[test]
fn coverage_and_unfit_examples_freeze_boundaries_without_modeling_runtime() {
    let fixture = fixture();
    let cases = &fixture["coverage_cases"];
    let advanced = case(cases, "replay_after_start_pruned");
    assert!(number(&advanced["start"]) < number(&advanced["floor"]));
    assert!(number(&advanced["floor"]) < number(&advanced["checkpoint"]));
    assert_eq!(advanced["expected"], "replay");
    let equal = case(cases, "replay_at_floor");
    assert_eq!(equal["checkpoint"], equal["floor"]);
    assert_eq!(equal["expected"], "replay");
    let expired = case(cases, "replay_below_floor");
    assert!(number(&expired["checkpoint"]) < number(&expired["floor"]));
    assert_eq!(expired["expected"], "rebuild");
    assert_eq!(case(cases, "scan_start_pruned")["expected"], "rebuild");

    let unfit = &fixture["unfit_summary"];
    let details = &unfit["response"]["error"]["details"];
    assert_eq!(unfit["response"]["error"]["code"], "payload_too_large");
    assert_eq!(details["record_kind"], "item");
    assert!(
        number(&details["minimum_required_bytes"]) > number(&details["maximum_response_bytes"])
    );
    assert_eq!(unfit["checkpoint_before"], unfit["checkpoint_after"]);
    assert_eq!(unfit["retry_same_budget_repairs"], false);
    assert_eq!(unfit["rebuild_same_budget_repairs"], false);
    assert_eq!(unfit["phases"], json!(["scan", "poll"]));
    // Reuse the existing response error detail type and wire names.
    let typed = bif::v2_response::PayloadTooLarge {
        record_kind: bif::v2_response::RecordKind::Item,
        record_id: text(&details["record_id"]).to_owned(),
        maximum_response_bytes: number(&details["maximum_response_bytes"]) as usize,
        minimum_required_bytes: number(&details["minimum_required_bytes"]) as usize,
    };
    assert_eq!(serde_json::to_value(typed).unwrap(), *details);
}

#[test]
fn lifecycle_schema_rejects_missing_fields_unknown_enums_and_extra_fields() {
    let fixture = fixture();
    let validator = validator();
    assert!(validator.is_valid(&fixture));
    // Every property of these representative closed objects is required.
    for pointer in [
        "/activation",
        "/activation/service_contract",
        "/activation/attempts/0",
        "/activation/attempts/0/prerequisites",
        "/activation/attempts/0/before",
        "/mirror",
        "/mirror/statuses/0",
        "/mirror/statuses/0/status",
        "/mirror/statuses/0/status/published_baseline",
        "/mirror/statuses/0/status/last_successful_catch_up",
        "/mirror/statuses/4/status/last_failure",
        "/mirror/transitions/0",
        "/mirror/callbacks/0",
        "/mirror/callbacks/0/captured",
        "/service_requests/0",
        "/service_requests/0/response",
        "/service_requests/0/response/error",
        "/service_requests/0/response/error/details",
        "/service_requests/4/response/error/details",
        "/unfit_summary",
        "/unfit_summary/response/error",
        "/unfit_summary/response/error/details",
        "/coverage_cases/0",
    ] {
        let object = fixture
            .pointer(pointer)
            .unwrap_or_else(|| panic!("missing {pointer}"));
        for field in object.as_object().expect("closed object").keys() {
            let mut changed = fixture.clone();
            changed
                .pointer_mut(pointer)
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            assert!(
                !validator.is_valid(&changed),
                "accepted omission {pointer}/{field}"
            );
        }
        let mut changed = fixture.clone();
        changed
            .pointer_mut(pointer)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert("extra".into(), json!(true));
        assert!(
            !validator.is_valid(&changed),
            "accepted extra field at {pointer}"
        );
    }
    for pointer in [
        "/activation/service_contract/mode",
        "/activation/attempts/0/action",
        "/activation/attempts/0/outcome",
        "/activation/attempts/0/rejection",
        "/mirror/statuses/0/status/rebuild_phase",
        "/mirror/statuses/0/status/source_compatibility",
        "/mirror/statuses/0/status/published_baseline/usability",
        "/mirror/statuses/4/status/last_failure/code",
        "/mirror/statuses/4/status/last_failure/recovery_action",
        "/mirror/callbacks/0/kind",
        "/mirror/callbacks/0/expected",
        "/service_requests/0/response/error/code",
        "/service_requests/0/response/error/details/reason",
        "/service_requests/4/response/error/details/reason",
        "/unfit_summary/response/error/details/record_kind",
        "/coverage_cases/0/phase",
        "/coverage_cases/0/expected",
    ] {
        let mut changed = fixture.clone();
        *changed.pointer_mut(pointer).unwrap() = json!("unknown");
        assert!(
            !validator.is_valid(&changed),
            "accepted unknown enum at {pointer}"
        );
    }
}

#[test]
fn sync_summary_matches_existing_domain_normalization_and_v2_serialization() {
    let item = ItemProjection::Summary(ItemSummary::new(
        ItemId::new(
            RequesterId::new("ALICE").unwrap(),
            ProjectId::new("bif").unwrap(),
            1,
        )
        .unwrap(),
        "A",
        Status::Ready,
        Some(Priority::P1),
        Some(AssigneeId::new("ALICE").unwrap()),
        Revision::new(3).unwrap(),
    ));
    let mut output = Vec::new();
    write_get(&mut output, &item, ResponseBudget::default()).unwrap();
    let response: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(
        fixture()["polls"][0]["response"]["result"]["replacements"][0]["item"],
        response["result"]["item"],
        "sync summaries must use the existing v2 wire representation"
    );
}

#[test]
fn all_fixture_item_identities_and_assignees_are_canonical() {
    fn check(value: &Value) {
        match value {
            Value::Object(object) => {
                for (field, value) in object {
                    let item_identity = field == "item_id"
                        || field == "scan_boundary"
                        || (field == "id" && object.contains_key("assignee"));
                    if item_identity && !value.is_null() {
                        let parts = text(value).split(':').collect::<Vec<_>>();
                        assert_eq!(parts.len(), 3, "item identity: {value}");
                        let id = ItemId::new(
                            RequesterId::new(parts[0]).unwrap(),
                            ProjectId::new(parts[1]).unwrap(),
                            parts[2].parse().unwrap(),
                        )
                        .unwrap();
                        assert_eq!(text(value), id.to_string(), "noncanonical {field}");
                    }
                    if field == "assignee" && !value.is_null() {
                        assert_eq!(
                            text(value),
                            AssigneeId::new(text(value)).unwrap().as_str(),
                            "noncanonical assignee"
                        );
                    }
                    check(value);
                }
            }
            Value::Array(values) => values.iter().for_each(check),
            _ => {}
        }
    }
    check(&fixture());
}

// Summary-only membership supplies neutral fields that these unfiltered views never inspect.
fn summary_domain_item(summary: &Value) -> Item {
    let parts = text(&summary["id"]).split(':').collect::<Vec<_>>();
    let id = ItemId::new(
        RequesterId::new(parts[0]).unwrap(),
        ProjectId::new(parts[1]).unwrap(),
        parts[2].parse().unwrap(),
    )
    .unwrap();
    let status = match text(&summary["status"]) {
        "proposed" => Status::Proposed,
        "ready" => Status::Ready,
        "in_progress" => Status::InProgress,
        "blocked" => Status::Blocked,
        "done" => Status::Done,
        "rejected" => Status::Rejected,
        other => panic!("unknown summary status {other}"),
    };
    let priority = summary["priority"].as_str().map(|priority| match priority {
        "P0" => Priority::P0,
        "P1" => Priority::P1,
        "P2" => Priority::P2,
        "P3" => Priority::P3,
        "P4" => Priority::P4,
        other => panic!("unknown summary priority {other}"),
    });
    Item::new(
        id,
        ItemContent::new(text(&summary["title"]), None, vec![]).unwrap(),
        status,
        priority,
        summary["assignee"]
            .as_str()
            .map(|value| AssigneeId::new(value).unwrap()),
        None,
        Revision::new(number(&summary["revision"])).unwrap(),
        Timestamp::new("model-only"),
        Timestamp::new("model-only"),
        Provenance::new(None, None, None, None, None, None, None),
    )
}

fn view_count<'a>(
    items: impl Iterator<Item = &'a Value>,
    requester: &RequesterId,
    view: NamedView,
) -> usize {
    let filters = EffectiveItemFilters::new(view, requester, &ItemListFilters::default()).unwrap();
    items
        .filter(|item| filters.matches(&summary_domain_item(item)))
        .count()
}

fn mine_count<'a>(items: impl Iterator<Item = &'a Value>, requester: &RequesterId) -> usize {
    view_count(items, requester, NamedView::Mine)
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CoveragePhase {
    Scan,
    Replay,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum CoverageDecision {
    Scan,
    Replay,
    Rebuild,
}

#[derive(Debug, serde::Deserialize)]
struct CoverageCase {
    phase: CoveragePhase,
    start: u64,
    checkpoint: u64,
    floor: u64,
    expected: CoverageDecision,
}

impl CoverageCase {
    // Positions are fixture/server semantics, not decoded client cursor tokens.
    fn decide(&self) -> CoverageDecision {
        match self.phase {
            CoveragePhase::Scan if self.start >= self.floor => CoverageDecision::Scan,
            CoveragePhase::Replay if self.checkpoint >= self.floor => CoverageDecision::Replay,
            _ => CoverageDecision::Rebuild,
        }
    }

    fn advance_to(&mut self, next: u64) -> bool {
        if next < self.checkpoint || self.decide() == CoverageDecision::Rebuild {
            return false;
        }
        self.checkpoint = next;
        true
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
struct CallbackGuard {
    attempt_id: String,
    baseline_identity: Option<String>,
    staging_identity: Option<String>,
    checkpoint: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum RebuildPhase {
    Idle,
    Scanning,
    CatchingUp,
    Blocked,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct FiniteCatchUp {
    checkpoint: String,
    high_water: u64,
    observation: String,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize)]
struct UnfitRecord {
    record_id: String,
    maximum_response_bytes: u64,
    minimum_required_bytes: u64,
}

#[derive(Clone, Debug, PartialEq)]
enum ModelFailure {
    Transient,
    Unfit(UnfitRecord),
}

#[derive(Clone, Debug, PartialEq)]
struct PublishedBaseline {
    identity: String,
    checkpoint: String,
    items: BTreeMap<String, Value>,
    complete: bool,
}

#[derive(Clone, Debug, PartialEq)]
struct Staging {
    identity: String,
    checkpoint: String,
    items: BTreeMap<String, Value>,
    high_water: Option<u64>,
    caught_up: Option<FiniteCatchUp>,
}

#[derive(Clone, Debug)]
enum PageProgress {
    Scan {
        exhausted: bool,
    },
    Replay {
        through: u64,
        high_water: u64,
        complete: bool,
        observation: String,
    },
}

#[derive(Clone, Debug)]
struct ModelPage {
    items: BTreeMap<String, Value>,
    checkpoint: String,
    progress: PageProgress,
}

#[derive(Clone, Debug)]
enum Callback {
    Page(ModelPage),
    Failure(ModelFailure),
}

#[derive(Debug, PartialEq)]
enum Delivery {
    Applied,
    Discarded,
    Blocked,
    Invalid,
}

#[derive(Debug, PartialEq)]
enum ModelError {
    NotOwner,
    Busy,
    Blocked,
    NotReady,
}

// Only ownership, callback guards and publication are modeled here. Revision/coalescing
// rules remain in ConsumerModel/check_poll; there is no transport, persistence or lock.
#[derive(Clone, Debug, PartialEq)]
struct PublicationModel {
    owner: Option<String>,
    serial: u64,
    pending: Option<CallbackGuard>,
    baseline: Option<PublishedBaseline>,
    staging: Option<Staging>,
    phase: RebuildPhase,
    last_success: Option<FiniteCatchUp>,
    failure: Option<ModelFailure>,
}

fn item_map(items: &[Value]) -> BTreeMap<String, Value> {
    items
        .iter()
        .map(|item| (text(&item["id"]).to_owned(), item.clone()))
        .collect()
}

impl PublicationModel {
    // Setup reads the amended examples; transitions never consult expected outcomes.
    fn from_status(fixture: &Value, id: &str, pending: Option<CallbackGuard>) -> Self {
        let status = &case(&fixture["mirror"]["statuses"], id)["status"];
        let baseline = &status["published_baseline"];
        let staging = status["staging_identity"].as_str().map(|identity| Staging {
            identity: identity.into(),
            checkpoint: pending.as_ref().unwrap().checkpoint.clone().unwrap(),
            items: BTreeMap::new(),
            high_water: None,
            caught_up: None,
        });
        Self {
            owner: Some("owner".into()),
            serial: 0,
            pending,
            baseline: (!baseline.is_null()).then(|| PublishedBaseline {
                identity: text(&baseline["identity"]).into(),
                checkpoint: text(&baseline["checkpoint"]).into(),
                items: item_map(if id == "published" {
                    array(&fixture["bootstrap"]["published_items"])
                } else {
                    array(&fixture["consumer_cases"][0]["before_items"])
                }),
                complete: baseline["complete"] == true,
            }),
            staging,
            phase: serde_json::from_value(status["rebuild_phase"].clone()).unwrap(),
            last_success: serde_json::from_value(status["last_successful_catch_up"].clone())
                .unwrap(),
            failure: match status["last_failure"]["code"].as_str() {
                None => None,
                Some("transient") => Some(ModelFailure::Transient),
                other => panic!("unsupported publication setup failure {other:?}"),
            },
        }
    }

    fn acquire(&mut self, owner: &str) -> Result<(), ModelError> {
        if self.owner.is_some() {
            return Err(ModelError::NotOwner);
        }
        self.owner = Some(owner.into());
        Ok(())
    }

    fn require_owner(&self, owner: &str) -> Result<(), ModelError> {
        if self.owner.as_deref() != Some(owner) {
            return Err(ModelError::NotOwner);
        }
        if self.phase == RebuildPhase::Blocked {
            return Err(ModelError::Blocked);
        }
        Ok(())
    }

    fn release(&mut self, owner: &str) -> Result<(), ModelError> {
        if self.owner.as_deref() != Some(owner) {
            return Err(ModelError::NotOwner);
        }
        self.owner = None;
        self.pending = None;
        Ok(())
    }

    fn begin_rebuild(
        &mut self,
        owner: &str,
        identity: &str,
        start: &str,
    ) -> Result<(), ModelError> {
        self.require_owner(owner)?;
        if self.pending.is_some() {
            return Err(ModelError::Busy);
        }
        self.staging = Some(Staging {
            identity: identity.into(),
            checkpoint: start.into(),
            items: BTreeMap::new(),
            high_water: None,
            caught_up: None,
        });
        self.phase = RebuildPhase::Scanning;
        Ok(())
    }

    fn guard(&self, attempt_id: String) -> CallbackGuard {
        CallbackGuard {
            attempt_id,
            baseline_identity: self
                .baseline
                .as_ref()
                .map(|baseline| baseline.identity.clone()),
            staging_identity: self
                .staging
                .as_ref()
                .map(|staging| staging.identity.clone()),
            checkpoint: self
                .staging
                .as_ref()
                .map(|stage| stage.checkpoint.clone())
                .or_else(|| {
                    self.baseline
                        .as_ref()
                        .map(|baseline| baseline.checkpoint.clone())
                }),
        }
    }

    fn request(&mut self, owner: &str) -> Result<CallbackGuard, ModelError> {
        self.require_owner(owner)?;
        if self.pending.is_some() {
            return Err(ModelError::Busy);
        }
        if self.staging.is_none() && self.baseline.is_none() {
            return Err(ModelError::NotReady);
        }
        self.serial += 1;
        let guard = self.guard(format!("model-attempt-{}", self.serial));
        self.pending = Some(guard.clone());
        Ok(guard)
    }

    // The same guard protects success and error. An accepted attempt is consumed,
    // including when no data/checkpoint changes, so a duplicate is not an observation.
    fn deliver(&mut self, captured: &CallbackGuard, callback: Callback) -> Delivery {
        if self.owner.is_none()
            || self.pending.as_ref() != Some(captured)
            || self.guard(captured.attempt_id.clone()) != *captured
        {
            return Delivery::Discarded;
        }
        let mut next = self.clone();
        let outcome = next.apply_callback(callback);
        if outcome != Delivery::Invalid {
            next.pending = None;
            *self = next;
        }
        outcome
    }

    fn apply_callback(&mut self, callback: Callback) -> Delivery {
        let page = match callback {
            Callback::Failure(failure) => {
                let blocked = matches!(failure, ModelFailure::Unfit(_));
                self.failure = Some(failure);
                if blocked {
                    self.phase = RebuildPhase::Blocked;
                    self.staging = None;
                }
                return if blocked {
                    Delivery::Blocked
                } else {
                    Delivery::Applied
                };
            }
            Callback::Page(page) => page,
        };
        let success = match page.progress {
            PageProgress::Scan { exhausted } => {
                if self.phase != RebuildPhase::Scanning {
                    return Delivery::Invalid;
                }
                if exhausted {
                    self.phase = RebuildPhase::CatchingUp;
                }
                None
            }
            PageProgress::Replay {
                through,
                high_water,
                complete,
                observation,
            } => {
                if !matches!(self.phase, RebuildPhase::Idle | RebuildPhase::CatchingUp)
                    || through > high_water
                    || complete != (through == high_water)
                {
                    return Delivery::Invalid;
                }
                if let Some(stage) = &mut self.staging {
                    if stage.high_water.is_some_and(|target| target != high_water) {
                        return Delivery::Invalid;
                    }
                    stage.high_water = Some(high_water);
                }
                complete.then(|| FiniteCatchUp {
                    checkpoint: page.checkpoint.clone(),
                    high_water,
                    observation,
                })
            }
        };
        if let Some(stage) = &mut self.staging {
            stage.items.extend(page.items);
            stage.checkpoint = page.checkpoint;
            stage.caught_up = success;
        } else if let Some(baseline) = &mut self.baseline {
            baseline.items.extend(page.items);
            baseline.checkpoint = page.checkpoint;
            if let Some(success) = success {
                self.last_success = Some(success);
                self.failure = None;
            }
        } else {
            return Delivery::Invalid;
        }
        Delivery::Applied
    }

    fn publish(&mut self, owner: &str, identity: &str) -> Result<(), ModelError> {
        self.require_owner(owner)?;
        let stage = self.staging.as_ref().ok_or(ModelError::NotReady)?;
        if stage.identity != identity
            || self.phase != RebuildPhase::CatchingUp
            || stage.caught_up.is_none()
            || self.pending.is_some()
        {
            return Err(ModelError::NotReady);
        }
        let stage = self.staging.take().unwrap();
        // One model transition replaces all publication coordinates, never unions old data.
        self.baseline = Some(PublishedBaseline {
            identity: stage.identity,
            checkpoint: stage.checkpoint,
            items: stage.items,
            complete: true,
        });
        self.last_success = stage.caught_up;
        self.failure = None;
        self.phase = RebuildPhase::Idle;
        Ok(())
    }
}

fn scan_page(fixture: &Value, index: usize) -> ModelPage {
    let result = &fixture["bootstrap"]["pages"][index]["response"]["result"];
    let exhausted = result["next_cursor"].is_null();
    ModelPage {
        items: item_map(array(&result["items"])),
        checkpoint: text(if exhausted {
            &result["sync_start_cursor"]
        } else {
            &result["next_cursor"]
        })
        .into(),
        progress: PageProgress::Scan { exhausted },
    }
}

fn model_page(fixture: &Value, complete: bool) -> ModelPage {
    let bootstrap = &fixture["bootstrap"];
    let high_water = number(&bootstrap["catch_up_high_water"]);
    ModelPage {
        items: item_map(array(&bootstrap["catch_up_replacements"])),
        checkpoint: if complete {
            text(&bootstrap["catch_up_checkpoint"]).into()
        } else {
            "partial-replay".into()
        },
        progress: PageProgress::Replay {
            through: if complete { high_water } else { high_water - 1 },
            high_water,
            complete,
            observation: "finite-catch-up-a".into(),
        },
    }
}

#[test]
fn mine_queue_matches_normalized_assignee_not_requester_casing() {
    let fixture = fixture();
    let requester = RequesterId::new("ALICE").unwrap();
    let assigned = array(&fixture["consumer_cases"][0]["before_items"]);
    assert_eq!(mine_count(assigned.iter(), &requester), 1);
    assert_eq!(
        mine_count(assigned.iter(), &RequesterId::new("BOB").unwrap()),
        0
    );
    let cleared = array(&fixture["consumer_cases"][0]["expected_items"]);
    assert_eq!(mine_count(cleared.iter(), &requester), 0);
}

#[test]
fn queue_membership_covers_every_status_and_owner() {
    let requester = RequesterId::new("alice").unwrap();
    let seed = fixture()["consumer_cases"][0]["before_items"][0].clone();
    // Expected outcomes are examples; the helper must use application semantics.
    for (status, ready, mine, active) in [
        ("proposed", false, true, false),
        ("ready", true, true, false),
        ("in_progress", false, true, true),
        ("blocked", false, true, true),
        ("done", false, false, false),
        ("rejected", false, false, false),
    ] {
        for assignee in [json!("alice"), json!("bob"), Value::Null] {
            let mut item = seed.clone();
            item["status"] = json!(status);
            item["assignee"] = assignee.clone();
            assert_eq!(
                mine_count(std::iter::once(&item), &requester),
                usize::from(mine && assignee == "alice"),
                "{status}/{assignee}"
            );
            for (view, expected) in [(NamedView::Ready, ready), (NamedView::Active, active)] {
                assert_eq!(
                    view_count(std::iter::once(&item), &requester, view),
                    usize::from(expected),
                    "{view}/{status}/{assignee}"
                );
            }
        }
    }
}

#[test]
fn coverage_decisions_execute_scan_start_and_durable_replay_boundaries() {
    for case in array(&fixture()["coverage_cases"]) {
        let mut coverage: CoverageCase = serde_json::from_value(case.clone()).unwrap();
        assert_eq!(coverage.decide(), coverage.expected, "{}", case["id"]);
        if coverage.expected == CoverageDecision::Rebuild {
            // A proposed advance across missing coverage must be rejected.
            let before = coverage.checkpoint;
            assert!(!coverage.advance_to(before + 1));
            assert_eq!(coverage.checkpoint, before);
        }
    }
}

#[test]
fn callback_fixture_examples_execute_without_changing_any_persisted_state() {
    let fixture = fixture();
    for case in array(&fixture["mirror"]["callbacks"]) {
        let captured = serde_json::from_value(case["captured"].clone()).unwrap();
        let persisted = serde_json::from_value(case["persisted_before"].clone()).unwrap();
        let mut model =
            PublicationModel::from_status(&fixture, text(&case["status_before"]), Some(persisted));
        let before = model.clone();
        let callback = if case["kind"] == "success" {
            Callback::Page(if model.phase == RebuildPhase::Scanning {
                scan_page(&fixture, 0)
            } else {
                model_page(&fixture, true)
            })
        } else {
            Callback::Failure(ModelFailure::Transient)
        };
        assert_eq!(model.deliver(&captured, callback), Delivery::Discarded);
        assert_eq!(model, before, "{}", case["id"]);
        assert_eq!(
            serde_json::to_value(model.pending.as_ref().unwrap()).unwrap(),
            case["persisted_after"]
        );
        assert_eq!(case["status_before"], case["status_after"]);
    }
}

#[test]
fn each_callback_guard_coordinate_blocks_both_success_and_failure() {
    let fixture = fixture();
    for coordinate in ["attempt", "baseline", "staging", "checkpoint", "owner"] {
        for callback in [
            Callback::Page(scan_page(&fixture, 0)),
            Callback::Failure(ModelFailure::Transient),
        ] {
            let mut model = PublicationModel::from_status(&fixture, "client-a-complete", None);
            model
                .begin_rebuild("owner", "stage-a", "scan-start")
                .unwrap();
            let captured = model.request("owner").unwrap();
            match coordinate {
                "attempt" => model.pending.as_mut().unwrap().attempt_id.push_str("-new"),
                "baseline" => model.baseline.as_mut().unwrap().identity.push_str("-new"),
                "staging" => model.staging.as_mut().unwrap().identity.push_str("-new"),
                "checkpoint" => model.staging.as_mut().unwrap().checkpoint.push_str("-new"),
                "owner" => {
                    model.release("owner").unwrap();
                    model.acquire("replacement-owner").unwrap();
                }
                _ => unreachable!(),
            }
            let before = model.clone();
            assert_eq!(model.deliver(&captured, callback), Delivery::Discarded);
            assert_eq!(model, before, "{coordinate}");
        }
    }
}

#[test]
fn bootstrap_publication_requires_one_owner_scan_exhaustion_and_finite_catch_up() {
    let fixture = fixture();
    let mut model = PublicationModel::from_status(&fixture, "client-a-complete", None);
    // This old-only row must disappear on replacement, not survive a union.
    model.baseline.as_mut().unwrap().items.insert(
        "ALICE:bif:999".into(),
        json!({"id": "ALICE:bif:999", "revision": 1}),
    );
    let old = model.baseline.clone();
    let previous_success = model.last_success.clone();
    assert_eq!(model.acquire("competitor"), Err(ModelError::NotOwner));
    model
        .begin_rebuild("owner", "stage-old", "scan-start")
        .unwrap();
    assert_eq!(
        model.publish("owner", "stage-old"),
        Err(ModelError::NotReady)
    );
    for page in [
        scan_page(&fixture, 0),
        scan_page(&fixture, 1),
        model_page(&fixture, true),
    ] {
        let request = model.request("owner").unwrap();
        assert_eq!(
            model.deliver(&request, Callback::Page(page)),
            Delivery::Applied
        );
    }
    assert!(model.staging.as_ref().unwrap().caught_up.is_some());
    let abandoned = model.request("owner").unwrap();
    assert_eq!(model.request("owner"), Err(ModelError::Busy));
    model.release("owner").unwrap();
    model.acquire("winner").unwrap();
    model
        .begin_rebuild("winner", "stage-new", "scan-start")
        .unwrap();
    let before = model.clone();
    assert_eq!(
        model.deliver(&abandoned, Callback::Page(model_page(&fixture, true))),
        Delivery::Discarded
    );
    assert_eq!(model, before);
    assert_eq!(
        model.publish("owner", "stage-old"),
        Err(ModelError::NotOwner)
    );
    assert_eq!(
        model.publish("winner", "stage-old"),
        Err(ModelError::NotReady)
    );

    let scan = model.request("winner").unwrap();
    assert_eq!(
        model.deliver(&scan, Callback::Page(scan_page(&fixture, 0))),
        Delivery::Applied
    );
    assert_eq!(
        model.publish("winner", "stage-new"),
        Err(ModelError::NotReady)
    );
    let scan = model.request("winner").unwrap();
    assert_eq!(
        model.deliver(&scan, Callback::Page(scan_page(&fixture, 1))),
        Delivery::Applied
    );
    assert_eq!(
        model.publish("winner", "stage-new"),
        Err(ModelError::NotReady)
    );
    let replay = model.request("winner").unwrap();
    assert_eq!(
        model.deliver(&replay, Callback::Page(model_page(&fixture, false))),
        Delivery::Applied
    );
    assert_eq!(model.baseline, old, "staging is not a published queue");
    assert_eq!(model.last_success, previous_success, "partial is not fresh");
    assert_eq!(
        model.publish("winner", "stage-new"),
        Err(ModelError::NotReady)
    );
    let replay = model.request("winner").unwrap();
    // A moving target or a false completion cannot authorize publication/freshness.
    for (through, high_water) in [(8, 8), (6, 7)] {
        let mut bad = model_page(&fixture, true);
        bad.progress = PageProgress::Replay {
            through,
            high_water,
            complete: true,
            observation: "regressed".into(),
        };
        let before = model.clone();
        assert_eq!(
            model.deliver(&replay, Callback::Page(bad)),
            Delivery::Invalid
        );
        assert_eq!(model, before);
    }
    assert_eq!(
        model.deliver(&replay, Callback::Page(model_page(&fixture, true))),
        Delivery::Applied
    );
    assert_eq!(model.baseline, old);
    model.publish("winner", "stage-new").unwrap();
    let published = model.baseline.as_ref().unwrap();
    assert_eq!(published.identity, "stage-new");
    assert!(published.complete);
    assert_eq!(published.checkpoint, "opaque-sync-7");
    assert_eq!(
        published.items,
        item_map(array(&fixture["bootstrap"]["published_items"]))
    );
    assert_eq!(model.phase, RebuildPhase::Idle);
    assert!(model.staging.is_none());
    assert_eq!(
        model.last_success.as_ref().unwrap().checkpoint,
        published.checkpoint
    );
    assert_eq!(model.last_success.as_ref().unwrap().high_water, 7);
    assert_eq!(
        model.publish("winner", "stage-old"),
        Err(ModelError::NotReady)
    );
    let published = model.clone();
    for callback in [
        Callback::Page(model_page(&fixture, true)),
        Callback::Failure(ModelFailure::Transient),
    ] {
        assert_eq!(model.deliver(&abandoned, callback), Delivery::Discarded);
        assert_eq!(
            model, published,
            "abandoned publisher cannot affect its replacement"
        );
    }
}

#[test]
fn no_change_callbacks_need_new_attempts_to_observe_freshness() {
    let fixture = fixture();
    let mut model = PublicationModel::from_status(&fixture, "transient-failure", None);
    let attempt = model.request("owner").unwrap();
    let checkpoint = model.baseline.as_ref().unwrap().checkpoint.clone();
    let page = ModelPage {
        items: BTreeMap::new(),
        checkpoint,
        progress: PageProgress::Replay {
            through: 3,
            high_water: 3,
            complete: true,
            observation: "new-no-change".into(),
        },
    };
    assert_eq!(
        model.deliver(&attempt, Callback::Page(page.clone())),
        Delivery::Applied
    );
    assert!(model.failure.is_none());
    assert_eq!(
        model.last_success.as_ref().unwrap().observation,
        "new-no-change"
    );
    let next = model.request("owner").unwrap();
    assert_ne!(attempt.attempt_id, next.attempt_id);
    assert_eq!(attempt.checkpoint, next.checkpoint);
    let observed = model.last_success.clone();
    assert_eq!(
        model.deliver(&next, Callback::Failure(ModelFailure::Transient)),
        Delivery::Applied
    );
    assert_eq!(
        model.last_success, observed,
        "failure retains the previous observation"
    );
    let before = model.clone();
    for callback in [
        Callback::Page(page),
        Callback::Failure(ModelFailure::Transient),
    ] {
        assert_eq!(model.deliver(&attempt, callback), Delivery::Discarded);
        assert_eq!(
            model, before,
            "matching checkpoint must not manufacture freshness"
        );
    }
}

#[test]
fn unfit_single_summary_blocks_scan_poll_retry_and_rebuild_without_partial_publish() {
    let fixture = fixture();
    let unfit: UnfitRecord =
        serde_json::from_value(fixture["unfit_summary"]["response"]["error"]["details"].clone())
            .unwrap();
    assert!(unfit.minimum_required_bytes > unfit.maximum_response_bytes);
    for phase in ["scan", "poll"] {
        let mut model = PublicationModel::from_status(&fixture, "client-a-complete", None);
        let baseline = model.baseline.clone();
        let success = model.last_success.clone();
        if phase == "scan" {
            model
                .begin_rebuild("owner", "stage-unfit", "scan-start")
                .unwrap();
            let attempt = model.request("owner").unwrap();
            assert_eq!(
                model.deliver(&attempt, Callback::Page(scan_page(&fixture, 0))),
                Delivery::Applied
            );
        }
        let attempt = model.request("owner").unwrap();
        assert_eq!(
            model.deliver(
                &attempt,
                Callback::Failure(ModelFailure::Unfit(unfit.clone()))
            ),
            Delivery::Blocked
        );
        assert_eq!(
            model.baseline, baseline,
            "{phase}: no advance or partial publish"
        );
        assert_eq!(model.last_success, success);
        assert_eq!(model.phase, RebuildPhase::Blocked);
        assert_eq!(model.failure, Some(ModelFailure::Unfit(unfit.clone())));
        let blocked = model.clone();
        assert_eq!(model.request("owner"), Err(ModelError::Blocked));
        assert_eq!(
            model.begin_rebuild("owner", "retry-stage", "scan-start"),
            Err(ModelError::Blocked)
        );
        assert_eq!(
            model.publish("owner", "stage-unfit"),
            Err(ModelError::Blocked)
        );
        assert_eq!(
            model, blocked,
            "unchanged record/budget cannot clear the blocker"
        );
        model.release("owner").unwrap();
        model.acquire("retry-owner").unwrap();
        assert_eq!(model.request("retry-owner"), Err(ModelError::Blocked));
        assert_eq!(model.last_success, success);
        assert_eq!(model.baseline, baseline);
    }
}

#[test]
fn strict_schema_accepts_fixture_and_rejects_wire_shape_drift() {
    let fixture = fixture();
    let validator = validator();
    assert!(
        validator.is_valid(&fixture),
        "{:?}",
        validator.iter_errors(&fixture).collect::<Vec<_>>()
    );
    for (pointer, value) in [
        ("/unexpected", json!(true)),
        ("/polls/0/request/view", json!("ready")),
        ("/polls/0/request/projection", json!("work")),
        ("/polls/0/request/limit", json!(101)),
        ("/polls/0/request/cursor", Value::Null),
        ("/polls/0/response/result/through", json!(-1)),
        ("/polls/0/response/result/high_water", json!("3")),
        ("/polls/0/response/result/complete", json!(1)),
        ("/polls/0/response/result/replacements/0/patch", json!({})),
        (
            "/polls/0/response/result/replacements/0/item/priority",
            json!("P5"),
        ),
        (
            "/polls/0/response/result/replacements/0/item/unknown",
            json!(true),
        ),
        (
            "/bootstrap/pages/0/response/result/items/0/revision",
            json!(0),
        ),
        ("/bootstrap/pages/0/request/offset", json!(1)),
        ("/tokens/0/unknown", json!(true)),
        (
            "/recovery/token_cases/8/error/details/reason",
            json!("wrong_generation"),
        ),
    ] {
        let mut changed = fixture.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        changed
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .insert(field.to_owned(), value);
        assert!(!validator.is_valid(&changed), "accepted drift at {pointer}");
    }
    for pointer in [
        "/polls/0/response/result/replacements/0/item/assignee",
        "/polls/0/response/result/source_checkpoint",
        "/bootstrap/pages/0/response/result/sync_start_cursor",
    ] {
        let mut changed = fixture.clone();
        let (parent, field) = pointer.rsplit_once('/').unwrap();
        changed
            .pointer_mut(parent)
            .unwrap()
            .as_object_mut()
            .unwrap()
            .remove(field);
        assert!(
            !validator.is_valid(&changed),
            "accepted omission at {pointer}"
        );
    }
}

#[test]
fn complete_summaries_reuse_the_frozen_read_contract() {
    let fixture = fixture();
    let read = document("bif-v2-read-contract.json");
    let expected = array(&read["projection_schemas"]["summary"]["fields"])
        .iter()
        .map(text)
        .collect::<BTreeSet<_>>();
    let mut items = Vec::new();
    for poll in array(&fixture["polls"]) {
        for replacement in array(&poll["response"]["result"]["replacements"]) {
            if replacement["kind"] == "upsert" {
                items.push(&replacement["item"]);
            }
        }
    }
    for page in array(&fixture["bootstrap"]["pages"]) {
        items.extend(array(&page["response"]["result"]["items"]));
    }
    assert!(!items.is_empty());
    for item in items {
        assert_eq!(
            item.as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<BTreeSet<_>>(),
            expected
        );
        assert!(array(&read["closed_enums"]["status"]).contains(&item["status"]));
        assert!(
            item["priority"].is_null()
                || array(&read["closed_enums"]["priority"]).contains(&item["priority"])
        );
        assert!(number(&item["revision"]) > 0);
    }
}

#[test]
fn effective_operations_append_once_replay_and_noop_never_advance() {
    let fixture = fixture();
    let journal = array(&fixture["journal"]);
    let mut high_water = 0;
    let mut operations = BTreeSet::new();
    let mut revisions = BTreeMap::new();
    for operation in array(&fixture["operations"]) {
        let append = operation["effective"] == true && operation["replay"] == false;
        let records = journal
            .iter()
            .filter(|record| record["operation_id"] == operation["id"])
            .collect::<Vec<_>>();
        assert_eq!(records.len(), usize::from(append));
        assert_eq!(
            number(&operation["before_revision"]),
            revisions
                .get(text(&operation["item_id"]))
                .copied()
                .unwrap_or(0),
            "timeline must start each operation at the committed item revision"
        );
        if append {
            assert!(operations.insert(text(&operation["id"])));
            let record = records[0];
            let seq = number(&record["change_seq"]);
            assert!(
                seq > high_water,
                "committed allocator must not reuse positions"
            );
            high_water = seq;
            assert_eq!(record["revision"], operation["after_revision"]);
            assert_eq!(record["item_id"], operation["item_id"]);
            assert_eq!(record["project"], operation["project"]);
            assert!(number(&operation["after_revision"]) > number(&operation["before_revision"]));
            revisions.insert(
                text(&operation["item_id"]),
                number(&operation["after_revision"]),
            );
        } else {
            assert_eq!(operation["before_revision"], operation["after_revision"]);
        }
        assert_eq!(number(&operation["expected_high_water"]), high_water);
    }
    assert_eq!(operations.len(), journal.len(), "no unlinked journal rows");
    for kind in [
        "note_only",
        "compound",
        "capture",
        "mutation",
        "noop",
        "replay",
    ] {
        assert!(
            array(&fixture["operations"])
                .iter()
                .any(|op| op["kind"] == kind)
        );
    }
    assert_eq!(
        high_water,
        number(&fixture["activation"]["after_operations_high_water"])
    );
}

fn token<'a>(fixture: &'a Value, handle: &Value) -> &'a Value {
    array(&fixture["tokens"])
        .iter()
        .find(|token| token["handle"] == *handle)
        .expect("illustrative opaque handle has model metadata")
}

// Derive coverage from retained matching rows, not the fixture's expected output.
fn check_poll(fixture: &Value, poll: &Value) -> Result<(), &'static str> {
    let request = &poll["request"];
    let source = token(fixture, &request["cursor"]);
    let result = &poll["response"]["result"];
    let next = token(fixture, &result["next_cursor"]);
    let c = number(&source["through"]);
    let h = source["high_water"]
        .as_u64()
        .unwrap_or(number(&poll["snapshot_high_water"]));
    let floor = number(&poll["snapshot_floor"]);
    if c < floor || c > h || h > number(&poll["snapshot_high_water"]) {
        return Err("invalid coverage");
    }
    let matching = array(&fixture["journal"])
        .iter()
        .filter(|row| {
            let seq = number(&row["change_seq"]);
            seq > c && seq <= h && row["project"] == request["project"]
        })
        .collect::<Vec<_>>();
    let budget = number(&poll["journal_limit"]).min(number(&request["limit"])) as usize;
    let consumed = matching.iter().take(budget).copied().collect::<Vec<_>>();
    let complete = matching.len() <= consumed.len();
    let through = if complete {
        h
    } else {
        number(&consumed.last().unwrap()["change_seq"])
    };
    if result["source_checkpoint"] != request["cursor"]
        || number(&result["high_water"]) != h
        || number(&result["through"]) != through
        || result["complete"] != complete
        || number(&next["through"]) != through
        || next["high_water"] != if complete { Value::Null } else { json!(h) }
    {
        return Err("incorrect fixed-window coverage");
    }
    for binding in [
        "store_id",
        "generation",
        "project",
        "projection_schema_version",
    ] {
        if source[binding] != next[binding] {
            return Err("changed cursor binding");
        }
    }
    if source["project"] != request["project"] || source["kind"] != "sync" || next["kind"] != "sync"
    {
        return Err("wrong scope or kind");
    }
    let ids = consumed
        .iter()
        .map(|row| text(&row["item_id"]))
        .collect::<BTreeSet<_>>();
    let replacements = array(&result["replacements"]);
    let actual = replacements
        .iter()
        .map(|row| text(&row["item"]["id"]))
        .collect::<BTreeSet<_>>();
    if actual != ids || actual.len() != replacements.len() {
        return Err("coalescing skipped or duplicated an item");
    }
    for replacement in replacements {
        let item = &replacement["item"];
        let current = array(&poll["snapshot_items"])
            .iter()
            .find(|row| row["id"] == item["id"]);
        if current != Some(item) {
            return Err("replacement is not complete current state");
        }
        let covered_revision = consumed
            .iter()
            .filter(|row| row["item_id"] == item["id"])
            .map(|row| number(&row["revision"]))
            .max()
            .unwrap();
        if number(&item["revision"]) < covered_revision {
            return Err("hydration regressed below covered operation");
        }
    }
    Ok(())
}

#[test]
fn fixed_windows_coalesce_without_skips_and_empty_projects_advance() {
    let fixture = fixture();
    let polls = array(&fixture["polls"]);
    let names = polls
        .iter()
        .map(|p| text(&p["id"]))
        .collect::<BTreeSet<_>>();
    for name in [
        "a_twice",
        "fixed_h_continuation",
        "no_changes",
        "other_project_only",
        "queue_exit",
    ] {
        assert!(names.contains(name), "missing hand-worked timeline {name}");
    }
    for poll in polls {
        assert_eq!(check_poll(&fixture, poll), Ok(()), "{}", poll["id"]);
    }
    let first = &polls[0];
    assert!(number(&first["response"]["result"]["replacements"][0]["item"]["revision"]) > 2);
    let continuation = &polls[1];
    assert_eq!(
        continuation["request"]["cursor"],
        first["response"]["result"]["next_cursor"]
    );
    assert!(
        number(&continuation["snapshot_high_water"])
            > number(&continuation["response"]["result"]["high_water"])
    );
    assert_eq!(
        continuation["response"]["result"]["replacements"][0]["item"]["revision"],
        4
    );
    for field in ["through", "high_water"] {
        let mut bad = first.clone();
        bad["response"]["result"][field] = json!(99);
        assert!(check_poll(&fixture, &bad).is_err());
    }
    let mut bad = first.clone();
    bad["response"]["result"]["replacements"] = json!([]);
    assert!(check_poll(&fixture, &bad).is_err());
    // Even a snapshot-consistent summary cannot acknowledge a newer covered revision.
    let mut bad = first.clone();
    bad["response"]["result"]["replacements"][0]["item"]["revision"] = json!(1);
    let regressed = bad["response"]["result"]["replacements"][0]["item"].clone();
    let snapshot = bad["snapshot_items"].as_array_mut().unwrap();
    let current = snapshot
        .iter_mut()
        .find(|item| item["id"] == regressed["id"])
        .unwrap();
    *current = regressed;
    assert_eq!(
        check_poll(&fixture, &bad),
        Err("hydration regressed below covered operation")
    );
}

#[test]
fn retention_floor_is_global_monotonic_and_prune_all_keeps_high_water() {
    let fixture = fixture();
    let mut floor = 0;
    let mut high_water = 0;
    for step in array(&fixture["retention"]["steps"]) {
        let next_floor = number(&step["floor"]);
        let next_high_water = number(&step["high_water"]);
        assert!(next_floor >= floor && next_high_water >= high_water);
        assert!(next_floor <= next_high_water);
        for seq in array(&step["retained_sequences"]).iter().map(number) {
            assert!(seq > next_floor && seq <= next_high_water);
        }
        for cursor in array(&step["cursors"]) {
            let c = number(&cursor["through"]);
            let expired = c < next_floor;
            assert!(c <= next_high_water, "future checkpoints are not valid");
            assert_eq!(
                cursor["expected"],
                if expired { "expired" } else { "valid" }
            );
        }
        if step["action"] == "prune_all" {
            assert_eq!(next_high_water, high_water);
            assert_eq!(next_floor, high_water);
            assert!(array(&step["retained_sequences"]).is_empty());
        }
        if step["action"] == "append_after_prune_all" {
            assert!(next_high_water > high_water);
        }
        floor = next_floor;
        high_water = next_high_water;
    }
    assert_eq!(fixture["retention"]["scope"], "store_global_sequence_space");
}

#[test]
fn bootstrap_captures_start_before_scan_and_replays_before_publish() {
    let fixture = fixture();
    let bootstrap = &fixture["bootstrap"];
    let start = token(&fixture, &bootstrap["starting_checkpoint"]);
    assert_eq!(start["high_water"], Value::Null);
    assert_eq!(
        number(&start["through"]),
        number(&bootstrap["captured_before_first_row"])
    );
    let mut baseline = BTreeMap::<String, Value>::new();
    let mut previous: Option<(String, String, u64)> = None;
    let pages = array(&bootstrap["pages"]);
    for (index, page) in pages.iter().enumerate() {
        let result = &page["response"]["result"];
        assert_eq!(
            result["sync_start_cursor"],
            bootstrap["starting_checkpoint"]
        );
        assert_eq!(page["request"]["project"], start["project"]);
        if !result["next_cursor"].is_null() {
            let continuation = token(&fixture, &result["next_cursor"]);
            assert_eq!(continuation["kind"], "scan");
            assert_eq!(
                continuation["sync_start_cursor"],
                bootstrap["starting_checkpoint"]
            );
            for binding in [
                "store_id",
                "generation",
                "project",
                "projection_schema_version",
            ] {
                assert_eq!(continuation[binding], start[binding]);
            }
            assert_eq!(
                continuation["scan_boundary"],
                array(&result["items"]).last().unwrap()["id"]
            );
        }
        assert_eq!(
            page["request"]["cursor"],
            if index == 0 {
                Value::Null
            } else {
                pages[index - 1]["response"]["result"]["next_cursor"].clone()
            }
        );
        for item in array(&result["items"]) {
            let parts = text(&item["id"]).split(':').collect::<Vec<_>>();
            let key = (
                parts[0].to_owned(),
                parts[1].to_owned(),
                parts[2].parse::<u64>().unwrap(),
            );
            assert!(previous.as_ref().is_none_or(|previous| previous < &key));
            previous = Some(key);
            baseline.insert(text(&item["id"]).to_owned(), item.clone());
        }
    }
    assert!(pages.last().unwrap()["response"]["result"]["next_cursor"].is_null());
    // Insert before the immutable boundary and update an already scanned item.
    assert_eq!(
        bootstrap["catch_up_source_checkpoint"],
        bootstrap["starting_checkpoint"]
    );
    let catch_up = token(&fixture, &bootstrap["catch_up_checkpoint"]);
    assert_eq!(bootstrap["catch_up_complete"], true);
    assert_eq!(catch_up["through"], bootstrap["catch_up_high_water"]);
    assert_eq!(catch_up["high_water"], Value::Null);
    let changed_ids = array(&fixture["journal"])
        .iter()
        .filter(|row| {
            row["project"] == start["project"]
                && number(&row["change_seq"]) > number(&start["through"])
                && number(&row["change_seq"]) <= number(&bootstrap["catch_up_high_water"])
        })
        .map(|row| text(&row["item_id"]))
        .collect::<BTreeSet<_>>();
    assert_eq!(
        array(&bootstrap["catch_up_replacements"])
            .iter()
            .map(|item| text(&item["id"]))
            .collect::<BTreeSet<_>>(),
        changed_ids
    );
    for item in array(&bootstrap["catch_up_replacements"]) {
        let id = text(&item["id"]).to_owned();
        if baseline
            .get(&id)
            .is_none_or(|old| number(&old["revision"]) < number(&item["revision"]))
        {
            baseline.insert(id, item.clone());
        }
    }
    let expected = array(&bootstrap["published_items"])
        .iter()
        .map(|item| (text(&item["id"]).to_owned(), item.clone()))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(baseline, expected);
    assert_eq!(bootstrap["publish_only_after_complete_catch_up"], true);
    assert_eq!(bootstrap["replace_old_baseline_not_merge"], true);
}

// Future deletion remains model-only; keep its revision after the live item is gone.
#[derive(Clone, Debug, PartialEq)]
enum ConsumerItemState {
    Live(Value),
    Tombstone(u64),
}

impl ConsumerItemState {
    fn revision(&self) -> u64 {
        match self {
            Self::Live(item) => number(&item["revision"]),
            Self::Tombstone(revision) => *revision,
        }
    }
}

// Test-only mirror state shared by fixture cases and sequential synthetic responses.
struct ConsumerModel {
    states: BTreeMap<String, ConsumerItemState>,
    checkpoint: Value,
    bootstrap_identity: Value,
    baseline_exists: bool,
}

impl ConsumerModel {
    fn from_case(case: &Value) -> Self {
        Self {
            states: array(&case["before_items"])
                .iter()
                .map(|item| {
                    (
                        text(&item["id"]).to_owned(),
                        ConsumerItemState::Live(item.clone()),
                    )
                })
                .collect(),
            checkpoint: case["persisted_checkpoint"].clone(),
            bootstrap_identity: case["bootstrap_identity"].clone(),
            baseline_exists: case["baseline_exists"] == true,
        }
    }

    // Queues and published items exclude tombstones, but revision checks do not.
    fn items(&self) -> BTreeMap<String, Value> {
        self.states
            .iter()
            .filter_map(|(id, state)| match state {
                ConsumerItemState::Live(item) => Some((id.clone(), item.clone())),
                ConsumerItemState::Tombstone(_) => None,
            })
            .collect()
    }

    fn apply(&mut self, case: &Value) -> &'static str {
        if !self.baseline_exists {
            return "bootstrap_required";
        }
        if self.checkpoint != case["response_source"]
            || self.bootstrap_identity != case["response_bootstrap_identity"]
        {
            return "discarded";
        }
        let mut states = self.states.clone();
        for replacement in array(&case["replacements"]) {
            let (id, state) = match text(&replacement["kind"]) {
                "upsert" => (
                    text(&replacement["item"]["id"]),
                    ConsumerItemState::Live(replacement["item"].clone()),
                ),
                "tombstone" => {
                    assert_eq!(case["future_domain"], true);
                    (
                        text(&replacement["item_id"]),
                        ConsumerItemState::Tombstone(number(&replacement["revision"])),
                    )
                }
                _ => panic!("unknown replacement kind"),
            };
            let revision = state.revision();
            if let Some(old) = states.get(id) {
                let old_revision = old.revision();
                if revision < old_revision {
                    continue;
                }
                if revision == old_revision {
                    if state != *old {
                        return "conflicting_same_revision";
                    }
                    continue;
                }
            }
            states.insert(id.to_owned(), state);
        }
        self.states = states;
        self.checkpoint = case["response_next"].clone();
        "committed"
    }
}

// Both responses use the currently persisted checkpoint and bootstrap identity:
// rejection must come from the retained item revision, not the stale-response guard.
fn check_consumer_tombstone_sequence(replacements: Value, expected_outcome: &str) {
    let fixture = fixture();
    let deletion = array(&fixture["consumer_cases"])
        .iter()
        .find(|case| case["id"] == "future_only_tombstone")
        .unwrap();
    let mut model = ConsumerModel::from_case(deletion);
    assert_eq!(model.apply(deletion), "committed");
    assert!(model.items().is_empty());
    assert_eq!(
        model.states,
        BTreeMap::from([("ALICE:bif:001".to_owned(), ConsumerItemState::Tombstone(4))])
    );
    assert_eq!(model.checkpoint, deletion["response_next"]);
    let before = model.states.clone();
    let checkpoint = model.checkpoint.clone();
    let follow_up = json!({
        "future_domain": true,
        "response_source": model.checkpoint,
        "response_bootstrap_identity": model.bootstrap_identity,
        "response_next": "opaque-future-next-checkpoint",
        "replacements": replacements,
    });
    assert_eq!(follow_up["response_source"], model.checkpoint);
    assert_eq!(
        follow_up["response_bootstrap_identity"],
        model.bootstrap_identity
    );
    assert_eq!(model.apply(&follow_up), expected_outcome);
    assert_eq!(
        model.states, before,
        "retained tombstone must not regress and conflicts must roll back all state"
    );
    assert!(model.items().is_empty());
    assert_eq!(
        model.checkpoint,
        if expected_outcome == "committed" {
            follow_up["response_next"].clone()
        } else {
            checkpoint
        }
    );
}

#[test]
fn consumer_tombstone_ignores_lower_revision_upsert_after_deletion() {
    let item = fixture()["consumer_cases"][0]["before_items"][0].clone();
    assert_eq!(item["revision"], 3);
    check_consumer_tombstone_sequence(json!([{"kind": "upsert", "item": item}]), "committed");
}

#[test]
fn consumer_tombstone_rejects_equal_revision_upsert_and_rolls_back_whole_response() {
    let mut item = fixture()["consumer_cases"][0]["before_items"][0].clone();
    item["revision"] = json!(4);
    let mut other = item.clone();
    other["id"] = json!("ALICE:bif:002");
    check_consumer_tombstone_sequence(
        json!([
            {"kind": "upsert", "item": other},
            {"kind": "tombstone", "item_id": "ALICE:bif:003", "revision": 5},
            {"kind": "upsert", "item": item},
        ]),
        "conflicting_same_revision",
    );
}

#[test]
fn consumer_tombstone_replays_identical_deletion_safely() {
    check_consumer_tombstone_sequence(
        json!([{"kind": "tombstone", "item_id": "ALICE:bif:001", "revision": 4}]),
        "committed",
    );
}

#[test]
fn consumer_tombstone_ignores_lower_revision_deletion() {
    check_consumer_tombstone_sequence(
        json!([{"kind": "tombstone", "item_id": "ALICE:bif:001", "revision": 3}]),
        "committed",
    );
}

#[test]
fn consumer_rejects_stale_responses_and_never_regresses_revisions_or_queues() {
    let fixture = fixture();
    for case in array(&fixture["consumer_cases"]) {
        let mut model = ConsumerModel::from_case(case);
        let outcome = model.apply(case);
        let items = model.items();
        assert_eq!(case["expected_outcome"], outcome, "{}", case["id"]);
        assert_eq!(model.checkpoint, case["expected_checkpoint"]);
        let expected = array(&case["expected_items"])
            .iter()
            .map(|item| (text(&item["id"]).to_owned(), item.clone()))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(items, expected, "{}", case["id"]);
        let requester = RequesterId::new("ALICE").unwrap();
        let ready = view_count(items.values(), &requester, NamedView::Ready);
        let mine = mine_count(items.values(), &requester);
        assert_eq!(ready as u64, number(&case["expected_ready_count"]));
        assert_eq!(mine as u64, number(&case["expected_mine_count"]));
        assert_eq!(
            view_count(items.values(), &requester, NamedView::Active),
            view_count(expected.values(), &requester, NamedView::Active),
            "active queue: {}",
            case["id"]
        );
    }
    assert_eq!(fixture["tombstones"]["runtime_enabled"], false);
    assert_eq!(
        fixture["tombstones"]["unexpected_missing_row"],
        "internal_corruption_no_cursor_advance"
    );
}

#[test]
fn recovery_rotates_independently_and_generationless_tokens_fail_explicitly() {
    let fixture = fixture();
    let recovery = &fixture["recovery"];
    let generations = array(&recovery["restores"])
        .iter()
        .map(|restore| text(&restore["new_generation"]))
        .collect::<BTreeSet<_>>();
    assert_eq!(generations.len(), array(&recovery["restores"]).len());
    for restore in array(&recovery["restores"]) {
        assert_ne!(restore["new_generation"], recovery["backup_generation"]);
        assert_eq!(restore["backup_id"], recovery["backup_id"]);
        for previous in array(&restore["rejected_generations"]) {
            assert_ne!(*previous, restore["new_generation"]);
        }
    }
    assert_eq!(recovery["generation_bits"], 128);
    assert_eq!(
        recovery["generation_source"],
        "independent_os_random_each_restore"
    );
    assert_eq!(
        recovery["collision_assumption"],
        "negligible_probability_not_absolute_uniqueness"
    );
    assert_eq!(recovery["ordinary_restart_preserves_generation"], true);
    let collision = &recovery["validator_collision"];
    assert_eq!(
        collision["before_item"]["id"],
        collision["restored_item"]["id"]
    );
    assert_eq!(
        collision["before_item"]["revision"],
        collision["restored_item"]["revision"]
    );
    assert_ne!(collision["before_item"], collision["restored_item"]);
    let mut rejected = BTreeSet::from([text(&recovery["backup_generation"])]);
    for restore in array(&recovery["restores"]) {
        assert_eq!(
            array(&restore["rejected_generations"])
                .iter()
                .map(text)
                .collect::<BTreeSet<_>>(),
            rejected,
            "repeated restore rejects every earlier generation"
        );
        rejected.insert(text(&restore["new_generation"]));
    }
    let cases = array(&recovery["token_cases"]);
    for kind in [
        "page",
        "history",
        "validator",
        "warm_session",
        "sync",
        "scan",
    ] {
        assert!(cases.iter().any(|case| case["kind"] == kind));
    }
    for case in cases {
        let missing = case["token_generation"].is_null();
        let changed = !missing && case["token_generation"] != case["store_generation"];
        if case["kind"] == "warm_session" {
            assert_eq!(
                case["outcome"],
                if changed {
                    "restart_required"
                } else {
                    "accepted"
                }
            );
        } else if case["kind"] == "validator" {
            if missing {
                assert_eq!(case["outcome"], "error");
                assert_eq!(case["error"]["code"], "invalid_input");
                assert_eq!(
                    case["error"]["details"],
                    json!({"reason": "generation_missing", "refetch_required": true})
                );
            } else {
                // A well-formed stale validator is a cache miss, not a cursor.
                assert_eq!(
                    case["outcome"],
                    if changed { "modified" } else { "accepted" }
                );
            }
        } else if missing || changed {
            assert_eq!(case["outcome"], "error");
            assert_eq!(case["error"]["code"], "invalid_cursor");
            assert_eq!(
                case["error"]["details"]["reason"],
                if missing {
                    "generation_missing"
                } else {
                    "wrong_generation"
                }
            );
            assert_eq!(case["error"]["details"]["restart_required"], true);
        } else {
            assert_eq!(case["outcome"], "accepted");
        }
        if case["outcome"] != "error" {
            assert!(case["error"].is_null());
        }
        assert_ne!(case["outcome"], "not_modified");
    }
}

#[test]
fn restored_collision_fixture_matches_runtime_conditional_read() {
    fn authorization() -> AuthorizationRequest<'static> {
        AuthorizationRequest {
            actor: Actor {
                kind: ActorKind::Human,
                id: "ALICE",
                surface: "cli",
                host: "local",
            },
            execution: Execution::Direct {
                surface: "cli",
                host: "local",
            },
            observed_execution: ObservedExecution::Direct,
            command: Command::Read,
            human_authorization: Some(HumanAuthorization::Direct { trusted: true }),
        }
    }

    // Allocate the identity and reach revision 3 through supported writes only.
    fn create_branch(path: &Path, summary: &Value) -> ItemId {
        let mut connection = storage::open(path).unwrap();
        let auth = authorization();
        let item = CaptureRepository::new(&mut connection)
            .unwrap()
            .capture(
                CaptureRequest {
                    idempotency_key: "capture".into(),
                    input: CaptureInput {
                        requester: RequesterId::new("ALICE").unwrap(),
                        project: ProjectId::new("bif").unwrap(),
                        content: ItemContent::new(
                            text(&summary["title"]),
                            None,
                            vec!["done".into()],
                        )
                        .unwrap(),
                        provenance: Provenance::default(),
                    },
                },
                "capture".into(),
                auth.actor,
                auth.execution,
                Timestamp::new("now"),
                CaptureIdentity {
                    operation_id: "capture".into(),
                    event_id: "capture".into(),
                },
            )
            .unwrap()
            .item;
        let mut repository = MutationRepository::new(&mut connection).unwrap();
        let mut item = item;
        for (key, mutation, event_count) in [
            (
                "approve",
                ItemMutation {
                    lifecycle: Some(LifecycleMutation::Approve),
                    triage: None,
                },
                1,
            ),
            (
                "triage",
                ItemMutation {
                    lifecycle: None,
                    triage: Some(Triage {
                        priority: TriageField::Set(Priority::P1),
                        assignee: TriageField::Set(
                            AssigneeId::new(text(&summary["assignee"])).unwrap(),
                        ),
                        note: None,
                    }),
                },
                2,
            ),
        ] {
            item = repository
                .mutate(
                    MutationRequest {
                        idempotency_key: key.into(),
                        item_id: item.id().clone(),
                        expected_revision: item.revision(),
                        mutation,
                    },
                    key.into(),
                    auth.actor,
                    auth.execution,
                    Timestamp::new("now"),
                    MutationIdentity {
                        operation_id: key.into(),
                        event_ids: (0..event_count)
                            .map(|index| format!("{key}-{index}"))
                            .collect(),
                    },
                )
                .unwrap()
                .item;
        }
        assert_eq!(item.id().to_string(), text(&summary["id"]));
        assert_eq!(item.revision().get(), number(&summary["revision"]));
        item.id().clone()
    }

    fn get(session: &mut ReadSession, id: &ItemId, version: Option<String>) -> Value {
        let bytes = session
            .execute(
                &authorization(),
                ReadRequest::Get {
                    item_id: id.clone(),
                    projection: ItemProjectionKind::Work,
                    known_version: version,
                    conditional: true,
                },
                ResponseBudget::default(),
            )
            .unwrap();
        assert!(session.is_autocommit());
        serde_json::from_slice(&bytes).unwrap()
    }

    fn complete_work(summary: &Value) -> Value {
        let mut work = summary.clone();
        let fields = work.as_object_mut().unwrap();
        fields.insert("description".into(), Value::Null);
        fields.insert("acceptance_criteria".into(), json!(["done"]));
        fields.insert("status_reason".into(), Value::Null);
        work
    }

    let fixture = fixture();
    let recovery = &fixture["recovery"];
    let case = array(&recovery["token_cases"])
        .iter()
        .find(|case| case["id"] == "validator_same_revision_different_restored_content")
        .expect("named validator collision fixture");
    let collision = &recovery["validator_collision"];
    let directory = tempfile::tempdir().unwrap();
    let config = Config {
        root: directory.path().to_owned(),
        requester: RequesterId::new("ALICE").unwrap(),
        source: None,
    };
    fs::create_dir(directory.path().join(".bif")).unwrap();
    let store = config.store_paths().unwrap().database;
    let before_allocation = directory.path().join("before-allocation.sqlite");
    let restored_branch = directory.path().join("restored-branch.sqlite");

    // Every connection/session is closed before maintenance touches the store.
    drop(storage::open(&store).unwrap());
    maintenance::backup(&store, &before_allocation).unwrap();
    let restored_id = create_branch(&store, &collision["restored_item"]);
    maintenance::backup(&store, &restored_branch).unwrap();
    maintenance::restore(
        &store,
        &before_allocation,
        directory.path().join("preserved-restored-branch.sqlite"),
    )
    .unwrap();
    let before_id = create_branch(&store, &collision["before_item"]);
    assert_eq!(before_id, restored_id);
    let before = get(
        &mut ReadSession::open(config.clone()).unwrap(),
        &before_id,
        None,
    );
    assert_eq!(
        before["result"]["item"],
        complete_work(&collision["before_item"])
    );
    let old_version = text(&before["result"]["version"]).to_owned();
    maintenance::restore(
        &store,
        &restored_branch,
        directory.path().join("preserved-later-branch.sqlite"),
    )
    .unwrap();

    let mut session = ReadSession::open(config).unwrap();
    let restored = get(&mut session, &restored_id, Some(old_version.clone()));
    assert_eq!(restored["ok"], true);
    assert!(
        restored.get("error").is_none(),
        "successful miss has no error"
    );
    assert_eq!(
        restored["result"]["item"],
        complete_work(&collision["restored_item"])
    );
    assert_eq!(
        before["result"]["item"]["id"],
        restored["result"]["item"]["id"]
    );
    assert_eq!(
        before["result"]["item"]["revision"],
        restored["result"]["item"]["revision"]
    );
    assert_ne!(before["result"]["item"], restored["result"]["item"]);
    let fresh_version = text(&restored["result"]["version"]).to_owned();
    assert_ne!(old_version, fresh_version);
    assert_ne!(
        old_version.rsplit_once('.').unwrap().1,
        fresh_version.rsplit_once('.').unwrap().1,
        "restore must rotate generation, not merely change the schema cookie"
    );
    let hit = get(&mut session, &restored_id, Some(fresh_version.clone()));
    assert_eq!(hit["result"]["outcome"], "not_modified");
    assert_eq!(hit["result"]["version"], fresh_version);
    assert!(hit["result"]["item"].is_null());

    // Generation-less legacy validators remain errors, unlike well-formed misses.
    let legacy = fresh_version.rsplit_once('.').unwrap().0;
    for version in [legacy.to_owned(), format!("{legacy}.null")] {
        let error = session
            .execute(
                &authorization(),
                ReadRequest::Get {
                    item_id: restored_id.clone(),
                    projection: ItemProjectionKind::Work,
                    known_version: Some(version),
                    conditional: true,
                },
                ResponseBudget::default(),
            )
            .unwrap_err();
        let error = serde_json::to_value(error).unwrap();
        assert_eq!(error["code"], "invalid_input");
        assert_eq!(
            error["details"],
            json!({"reason": "generation_missing", "refetch_required": true})
        );
    }
    assert_eq!(
        restored["result"]["outcome"], case["outcome"],
        "runtime outcome must agree with the named recovery fixture"
    );
    assert_eq!(restored["error"], case["error"]);
}

#[test]
fn one_read_snapshot_prevents_generation_or_retention_gaps_and_scope_cycles() {
    let fixture = fixture();
    for case in array(&fixture["read_interleavings"]) {
        let snapshot = &case["snapshot"];
        for read in [
            "generation_read",
            "floor_read",
            "coverage_read",
            "rows_read",
        ] {
            assert_eq!(case[read], snapshot["transaction_id"]);
        }
        let retained = array(&fixture["journal"])
            .iter()
            .map(|row| number(&row["change_seq"]))
            .filter(|seq| {
                *seq > number(&snapshot["floor"]) && *seq <= number(&snapshot["high_water"])
            })
            .map(|seq| json!(seq))
            .collect::<Vec<_>>();
        assert_eq!(array(&snapshot["covered_sequences"]), retained);
        let expired = number(&case["cursor_through"]) < number(&snapshot["floor"]);
        let changed = case["cursor_generation"] != snapshot["generation"];
        let expected = if changed {
            "wrong_generation"
        } else if expired {
            "expired"
        } else {
            "complete_coverage"
        };
        assert_eq!(case["expected"], expected);
        if expected == "complete_coverage" {
            assert_eq!(case["selected_sequences"], snapshot["covered_sequences"]);
        } else {
            assert!(array(&case["selected_sequences"]).is_empty());
        }
    }
    let scope = &fixture["validation_scope"];
    assert_eq!(
        scope["phase_e_runtime"],
        json!([
            "existing_page_history_validator_warm_session_invalidation",
            "storage_read_interleavings"
        ])
    );
    assert_eq!(
        scope["phase_f_runtime"],
        json!([
            "V2-036_sync_expiry",
            "V2-039_bootstrap",
            "later_reconciliation_integration"
        ])
    );
    assert_eq!(scope["v2_035_depends_on_v2_037"], false);
    assert_eq!(fixture["activation"]["initial_high_water"], 0);
    assert_eq!(fixture["activation"]["initial_floor"], 0);
    assert_eq!(fixture["activation"]["timestamp_audit_backfill"], false);
    assert_eq!(
        fixture["activation"]["sync_enabled_before_all_writers_participate"],
        false
    );
}
