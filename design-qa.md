# Settings navigation redesign — Branch 3 evidence

## Scope

- Four primary destinations: Transcribe, Models, History, Settings.
- Four Settings tabs: General, Recording, Advanced, About.
- Output is a legacy route normalized to General; About and developer tooling live under Settings.

## Source-level evidence

- Sidebar source renders the four destinations in the required order without a More disclosure.
- Settings tab source renders exactly General, Recording, Advanced, and About.
- The passive meter predicate is centralized and requires a visible Settings > Recording route with no active/deferred capture or competing playback owner.
- Passive meter repainting is fast only while its session exists; it is stopped on route exit, hiding, quitting, or capture ownership.

## Automated verification

- `cargo fmt --all --check`: passed.
- `git diff --check`: passed.
- `cargo check --all-targets --all-features`: blocked before crate compilation because this environment's CMake does not provide the required `Visual Studio 17 2022` generator for `transcribe-cpp-sys`.

## Native verification pending

- No real microphone, speech, runtime, download, network, or desktop smoke test was run.
- Capture/meter lifecycle remains pending native verification on a Windows machine with the required C++ build tooling.
