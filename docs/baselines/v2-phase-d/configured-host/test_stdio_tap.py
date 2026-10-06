"""Focused checks for recording transport, without an MCP client or BIF store."""

import hashlib
import json
import os
from pathlib import Path
import select
import signal
import subprocess
import sys
import tempfile
import time
import unittest


TAP = Path(__file__).with_name("stdio_tap.py")


class TransportRecorderTests(unittest.TestCase):
    """The recorder must preserve bytes, provenance, and child cleanup."""

    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def launch(self, program, digest=None, stdout=subprocess.PIPE):
        config = self.root / "launch.json"
        config.write_text(json.dumps({
            "argv": [sys.executable, "-u", "-c", program],
            "binary_sha256": digest or hashlib.sha256(Path(sys.executable).read_bytes()).hexdigest(),
            "cwd": str(self.root),
            "env": {"PATH": "/usr/bin:/bin"},
            "transcript_directory": str(self.root / "transcripts"),
        }))
        process = subprocess.Popen(
            [sys.executable, str(TAP), str(config)],
            stdin=subprocess.PIPE, stdout=stdout, stderr=subprocess.PIPE,
        )
        self.addCleanup(self.stop, process)
        return process

    @staticmethod
    def stop(process):
        try:
            # Fixture-child cleanup runs first; allow the recorder to reap it.
            process.communicate(timeout=1)
        except subprocess.TimeoutExpired:
            process.kill()
            process.communicate(timeout=5)

    def session(self):
        sessions = list((self.root / "transcripts").glob("session-*"))
        self.assertEqual(len(sessions), 1)
        return sessions[0]

    def launch_uncooperative_child(self, output_size=0):
        """Readiness and cleanup do not depend on the recorder forwarding stdout."""
        ready = self.root / "child-ready"
        process = self.launch(
            "import os, signal, time; from pathlib import Path; "
            "signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            f"Path({str(ready)!r}).write_text(str(os.getpid())); "
            f"payload = b'x' * {output_size}; "
            "exec('while payload:\\n written = os.write(1, payload)\\n payload = payload[written:]'); "
            "time.sleep(30)"
        )
        self.addCleanup(self.stop_fixture_child, ready)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            sessions = list((self.root / "transcripts").glob("session-*/session.json"))
            if ready.exists() and ready.read_text() and sessions:
                return process, int(ready.read_text())
            time.sleep(0.01)
        self.fail("synthetic child did not become ready")

    @staticmethod
    def stop_fixture_child(ready):
        """Kill only this fixture's child, even when the old recorder hangs."""
        if ready.exists() and ready.read_text():
            try:
                os.kill(int(ready.read_text()), signal.SIGKILL)
            except ProcessLookupError:
                pass

    def assert_forced_shutdown(self, process, child_pid):
        """Leave host stdin open; reaping and metadata must not require pipe EOF."""
        started = time.monotonic()
        process.terminate()
        process.wait(timeout=6)
        self.assertLess(time.monotonic() - started, 6)
        process.communicate(timeout=1)
        metadata = json.loads((self.session() / "session.json").read_text())
        self.assertEqual(metadata["exit_code"], -signal.SIGKILL)
        self.assertEqual(metadata["received_signals"], [signal.SIGTERM])
        self.assertTrue(metadata["ended_at"])
        self.assertEqual(process.returncode, 1 if metadata["relay_errors"] else 128 + signal.SIGKILL)
        with self.assertRaises(ProcessLookupError):
            os.kill(child_pid, 0)
        return metadata

    def test_ignored_sigterm_is_escalated_without_waiting_for_stdout_eof(self):
        process, child_pid = self.launch_uncooperative_child()
        metadata = self.assert_forced_shutdown(process, child_pid)
        self.assertEqual(metadata["relay_errors"], [])

    def test_shutdown_is_bounded_when_host_does_not_drain_stdout(self):
        process, child_pid = self.launch_uncooperative_child(262144)
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            captures = list((self.root / "transcripts").glob("session-*/server-to-client.bin"))
            if captures and captures[0].stat().st_size >= 65536:
                break
            time.sleep(0.01)
        else:
            self.fail("synthetic child did not fill the output relay")
        metadata = self.assert_forced_shutdown(process, child_pid)
        self.assertIn("stdout relay did not drain before shutdown", metadata["relay_errors"])

    def test_normal_child_exit_does_not_wait_for_host_stdin_eof(self):
        process = self.launch("import os; os.write(1, b'final bytes')")
        process.wait(timeout=5)
        stdout, stderr = process.communicate(timeout=1)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(stdout, b"final bytes")
        metadata = json.loads((self.session() / "session.json").read_text())
        self.assertTrue(metadata["ended_at"])
        self.assertEqual(metadata["exit_code"], 0)
        self.assertEqual(metadata["relay_errors"], [])

    def test_normal_exit_preserves_bytes_when_host_resumes_reading_later(self):
        read_fd, write_fd = os.pipe()
        self.addCleanup(os.close, read_fd)
        os.set_blocking(write_fd, False)
        filled = 0
        try:
            while True:
                filled += os.write(write_fd, b"x" * 4096)
        except BlockingIOError:
            pass
        ready = self.root / "normal-child-ready"
        try:
            process = self.launch(
                "import os; from pathlib import Path; os.write(1, b'final bytes'); "
                f"Path({str(ready)!r}).touch()", stdout=write_fd,
            )
        finally:
            os.close(write_fd)
        deadline = time.monotonic() + 5
        while not ready.exists():
            self.assertLess(time.monotonic(), deadline, "normal child did not become ready")
            time.sleep(0.01)
        # The child can exit while its last bytes are blocked behind the host's full pipe.
        time.sleep(1.5)
        prefix = bytearray()
        while len(prefix) < filled:
            self.assertTrue(select.select([read_fd], [], [], 5)[0], "host prefix did not drain")
            chunk = os.read(read_fd, filled - len(prefix))
            self.assertTrue(chunk, "host pipe closed before prefix drained")
            prefix.extend(chunk)
        self.assertEqual(prefix, b"x" * filled)
        process.wait(timeout=5)
        self.assertTrue(select.select([read_fd], [], [], 1)[0], "final bytes did not arrive")
        self.assertEqual(os.read(read_fd, 64), b"final bytes")
        _, stderr = process.communicate(timeout=1)
        self.assertEqual(process.returncode, 0, stderr)
        metadata = json.loads((self.session() / "session.json").read_text())
        self.assertEqual(metadata["relay_errors"], [])

    def test_records_and_forwards_large_binary_streams_without_inventing_bytes(self):
        process = self.launch(
            "import os; os.write(2, b'child diagnostic\\n'); "
            "exec('while True:\\n data = os.read(0, 65536)\\n if not data: break\\n os.write(1, data)')"
        )
        payload = bytes(range(256)) * 4096 + b"unterminated final fragment"
        stdout, stderr = process.communicate(payload, timeout=10)
        self.assertEqual(process.returncode, 0, stderr)
        self.assertEqual(stdout, payload)
        self.assertEqual(stderr, b"child diagnostic\n")
        session = self.session()
        self.assertEqual((session / "client-to-server.bin").read_bytes(), payload)
        self.assertEqual((session / "server-to-client.bin").read_bytes(), payload)
        self.assertEqual((session / "server-stderr.bin").read_bytes(), stderr)
        metadata = json.loads((session / "session.json").read_text())
        self.assertEqual(metadata["exit_code"], 0)
        self.assertEqual(metadata["relay_errors"], [])
        self.assertTrue(metadata["ended_at"])
        self.assertEqual(metadata["environment"], {"PATH": "/usr/bin:/bin"})
        self.assertEqual(metadata["launcher_sha256"], hashlib.sha256(TAP.read_bytes()).hexdigest())

    def test_changed_binary_is_refused_before_launch(self):
        process = self.launch("print('must not execute')", digest="0" * 64)
        stdout, stderr = process.communicate(timeout=5)
        self.assertNotEqual(process.returncode, 0)
        self.assertEqual(stdout, b"")
        self.assertIn(b"binary hash mismatch", stderr)
        self.assertFalse((self.root / "transcripts").exists())

    def test_host_termination_stops_the_child_and_records_exit(self):
        process = self.launch("import os, time; os.write(1, b'ready\\n'); time.sleep(30)")
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            sessions = list((self.root / "transcripts").glob("session-*/session.json"))
            if sessions and json.loads(sessions[0].read_text()).get("server_pid"):
                break
            time.sleep(0.01)
        else:
            self.fail("child did not start")
        # Wait until the child is ready before terminating the application-owned launcher.
        self.assertEqual(process.stdout.readline(), b"ready\n")
        process.terminate()
        process.communicate(timeout=5)
        metadata = json.loads((self.session() / "session.json").read_text())
        self.assertEqual(metadata["exit_code"], -signal.SIGTERM)
        self.assertEqual(metadata["received_signals"], [signal.SIGTERM])
        self.assertEqual(metadata["relay_errors"], [])
        self.assertTrue(metadata["ended_at"])
        with self.assertRaises(ProcessLookupError):
            os.kill(metadata["server_pid"], 0)


if __name__ == "__main__":
    unittest.main()
