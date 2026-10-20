# engine-iso 0.1.0

The community ISO 9613-2:2024 calculation engine for baseline Intel/AMD x86-64.
No Git or compiler is required. Each archive holds only the engine executable
and `LICENSES.txt` (its license, third-party notices and source offer).

| Computer | Download | Engine executable |
| --- | --- | --- |
| Linux x86-64 | [engine-iso-0.1.0-linux-x86_64.tar.gz](https://github.com/coloradoanalytics/engine-iso/raw/refs/heads/main/releases/0.1.0/engine-iso-0.1.0-linux-x86_64.tar.gz) | `engine-iso-0.1.0` (static musl) |
| Windows x86-64 | [engine-iso-0.1.0-windows-x86_64.zip](https://github.com/coloradoanalytics/engine-iso/raw/refs/heads/main/releases/0.1.0/engine-iso-0.1.0-windows-x86_64.zip) | `engine-iso-0.1.0.exe` (Windows system DLLs) |

Extract the archive into the `engines` folder beside `acalc-engine` (Windows:
`acalc-engine.exe`), keeping the archive's own folder, so the engine is at
`engines/engine-iso-0.1.0-linux-x86_64/engine-iso-0.1.0` (Windows:
`engines\engine-iso-0.1.0-windows-x86_64\engine-iso-0.1.0.exe`). On Linux,
`tar -xzf engine-iso-0.1.0-linux-x86_64.tar.gz -C engines`; on Windows, **Extract All** into `engines`. Start
acalc-engine; it finds the engine at startup and starts it when needed. If
several engine-iso versions are installed, choose one on its **Engines** page;
use **Refresh** there after adding an engine while the service runs.
The complete guide is the
[engine-iso README](https://github.com/coloradoanalytics/engine-iso#download-and-install).

`SHA256SUMS` identifies the archives. On Linux, run `sha256sum -c SHA256SUMS`
with both archives in the same folder. On Windows, use
`Get-FileHash .\engine-iso-0.1.0-windows-x86_64.zip -Algorithm SHA256` and compare the hash with
`SHA256SUMS`. `BUILD-INFO.json` records the source file hashes,
compiler, targets, engine identity and executable and archive hashes; its
one-worker identity is only for reproducibility.

This is an engineering implementation, not a verified ISO conformity release.
Use the same target, build and math library on all nodes of a calculation farm.
Corresponding source: <https://github.com/coloradoanalytics/engine-iso>, the commit whose source files match
`source_files_sha256` in `BUILD-INFO.json`.
