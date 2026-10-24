"""PRE-SIGN CPU entry from the SAME fully verified Metal-enabled app.

Distinct from the unchanged CPU-only public constructor. No stripped app,
preverified input, signing, package authentication or installation is accepted.
"""
from pathlib import Path

import macos_cpu_component as component
import macos_cpu_pkg_entry as cpu
import macos_full_release as full


def build_entry(app: Path, workspace: Path, frozen: full.FrozenInputs) -> tuple[Path, str, int]:
    """Independently admit a full release, then embed only its exact CPU worker."""
    with full.owned_release(app, workspace, frozen) as owned:
        worker = owned.copied / component.CPU_APP_INPUTS[0]
        candidate, sha, size = cpu._compile_owned_worker(worker, owned.private,
                                                        frozen.cpu_worker_sha256, frozen.revision)
        identity = component.fingerprint(candidate.lstat())
        owned.check_unchanged()
        component.require(component.hash_file(candidate, cpu.MAX_ENTRY) == (sha, size)
                          and component.fingerprint(candidate.lstat()) == identity,
                          "CPU entry changed during final full-release binding")
        component.publish_no_replace(candidate, owned.output)
        output = owned.output
    return output, sha, size
