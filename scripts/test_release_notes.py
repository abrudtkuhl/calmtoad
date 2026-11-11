import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

from release_notes import extract_notes


class ReleaseNotesTests(unittest.TestCase):
    def test_exact_version_and_section_boundaries(self):
        changelog = """# Changelog
## [Unreleased]
### Added
- Future feature.
## [0.1.10]
### Fixed
- Other version.
## [0.1.1] - 2026-11-11
### Improved
- Larger crawls.
  With a continuation.
## [0.1.0]
### Added
- Earlier feature.
"""
        self.assertEqual(extract_notes(changelog, "0.1.1"),
                         "### Improved\n- Larger crawls.\n  With a continuation.\n")

    def test_final_section_crlf_and_prerelease(self):
        self.assertEqual(extract_notes("## [1.0.0-rc.1+build.2]\r\n\r\n### Fixed\r\n* A fix.\r\n",
                                       "1.0.0-rc.1+build.2"), "### Fixed\n* A fix.\n")

    def test_rejects_missing_duplicate_empty_and_uncategorized(self):
        for body in ("", "## [0.1.10]\n### Fixed\n- Other.",
                     "## [0.1.1]\n## [0.1.1]\n### Fixed\n- Duplicate.",
                     "## [0.1.1]\n", "## [0.1.1]\n### Added\n### Fixed\n",
                     "## [0.1.1]\n### Fixed\n<!-- - Guidance only. -->\n",
                     "## [0.1.1]\n### Fixed\n-   \n",
                     "## [0.1.1]\n- No category.",
                     "## [0.1.1]\n### Internal\n- Implementation detail."):
            with self.subTest(body=body), self.assertRaises(ValueError):
                extract_notes(body, "0.1.1")

    def test_rejects_invalid_requested_versions(self):
        for version in ("v0.1.1", "", "0.1", "0.1.1\n", "../0.1.1"):
            with self.subTest(version=version), self.assertRaises(ValueError):
                extract_notes("## [0.1.1]\n### Added\n- Feature.", version)

    def test_cli_success_and_failure_have_separate_output(self):
        with tempfile.TemporaryDirectory() as directory:
            changelog = Path(directory) / "CHANGELOG.md"
            changelog.write_text("## [0.1.1]\n### Fixed\n- A fix.\n", encoding="utf-8")
            command = [sys.executable, str(Path(__file__).with_name("release_notes.py")),
                       "0.1.1", "--changelog", str(changelog)]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0)
            self.assertEqual(result.stdout, "### Fixed\n- A fix.\n")
            self.assertEqual(result.stderr, "")
            changelog.write_text("## [0.1.1]\n", encoding="utf-8")
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")
            self.assertIn("user-facing bullet", result.stderr)
            changelog.unlink()
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
