# Tests scripts/check-contract.py against made-up versions of main's
# contract/contract.version and this one, in a scratch repository.

import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

script = pathlib.Path(__file__).resolve().parent / "check-contract.py"
failed = False

with tempfile.TemporaryDirectory(prefix="ritornel-check-contract-") as scratch:
    scratch = pathlib.Path(scratch)

    def git(*args: str) -> None:
        # Settings of the scratch repository's own, whatever a person's
        # Git is set to: no line endings changed, nothing signed.
        isolated = ["-c", "core.autocrlf=false", "-c", "commit.gpgsign=false", "-c", "tag.gpgsign=false"]
        subprocess.run(["git", "-C", str(scratch), *isolated, *args], check=True, stdout=subprocess.DEVNULL)

    def version_file(version: int, digest: str) -> None:
        text = f"# A comment.\nversion {version}\ndigest {digest}\n"
        (scratch / "contract" / "contract.version").write_text(text, encoding="utf-8", newline="\n")

    def check(base: str) -> bool:
        """Whether check-contract.py passes against `base`."""
        done = subprocess.run(
            [sys.executable, str(scratch / "scripts" / "check-contract.py")],
            env={**os.environ, "RITORNEL_CONTRACT_BASE": base},
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        return done.returncode == 0

    git("init", "--quiet")
    (scratch / "scripts").mkdir()
    (scratch / "contract").mkdir()
    shutil.copy(script, scratch / "scripts")
    version_file(2, "aaaa")
    git("add", "contract")
    git("-c", "user.name=test", "-c", "user.email=test@example.invalid", "commit", "--quiet", "-m", "main")
    git("tag", "base")

    def expect(want: bool, version: int, digest: str, what: str) -> None:
        global failed
        version_file(version, digest)
        if check("base") != want:
            print(f"error: check-contract.py should {'pass' if want else 'fail'} {what}, and did not", file=sys.stderr)
            failed = True

    expect(True, 2, "aaaa", "the same wire at main's version")
    expect(False, 2, "bbbb", "another wire at main's version")
    expect(True, 3, "bbbb", "another wire one version above main's")
    expect(True, 3, "cccc", "a wire changed again one version above main's")
    expect(False, 4, "bbbb", "a second raise")
    expect(False, 1, "aaaa", "a version behind main's")

    if check("nowhere"):
        print("error: check-contract.py should fail with no base to read", file=sys.stderr)
        failed = True

sys.exit(1 if failed else 0)
