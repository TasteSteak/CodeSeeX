# Installer Migration

CodeSeeX 0.5 keeps the product identity from the Electron line while moving the desktop runtime to Tauri 2. The Windows NSIS installer is the primary user-facing installer because it can preserve the assisted install experience and run one-time legacy cleanup.

## Windows EXE

The NSIS EXE uses a checked-in template at `apps/desktop/src-tauri/installer/windows/installer.nsi`.

User-facing flow:

1. Select installer language from the languages supported by the desktop client.
2. Select current-user or all-users install mode.
3. Detect an existing CodeSeeX install.
4. For legacy Electron installs, run the legacy uninstaller, clean residual installer state, then install the Tauri build.
5. For Tauri-to-Tauri installs, use the normal repair/update flow.

Detection order:

- Tauri NSIS uninstall key: `Software\Microsoft\Windows\CurrentVersion\Uninstall\CodeSeeX`
- Legacy Electron uninstall key: `Software\Microsoft\Windows\CurrentVersion\Uninstall\ef214345-602f-584f-9f76-9aebcb3a1dcf`
- The legacy key is checked under both `HKCU` and `HKLM` so current-user and all-users migrations are both visible.

Legacy cleanup is intentionally conservative. The installer calls the legacy uninstaller first, then removes legacy shortcuts, legacy uninstall registry keys, the autostart value, empty legacy install directories, and updater cache. It does not delete `~/.codeseex` or other user-owned runtime data.

## macOS And Linux

macOS and Linux packaging should match the Windows outcome rather than the exact Windows page sequence:

- Keep the product name `CodeSeeX` and identifier `io.github.tastesteak.codeseex`.
- Preserve `~/.codeseex`.
- Prefer first-run application migration for platform-specific residual cleanup that cannot be expressed safely in DMG/AppImage-style installers.
- Keep package-manager semantics for Linux packages instead of simulating a Windows install-mode selector.
