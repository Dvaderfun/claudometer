# Vibecode recovery

The wake lock and persistent lid override are independent. Closing Claudometer
always drops the wake lock first. Persistent recovery can remain outstanding
without falsely reporting the wake lock as failed or active.

## Automatic recovery

On startup Claudometer processes `power-override.v1.json` before applying any
new lid override. The journal identifies the exact power scheme and original
and applied AC/DC values. Recovery:

1. records `restoring` durably;
2. restores a field only if it still equals Claudometer's applied value;
3. treats an already-original field as restored;
4. relinquishes a field whose value was changed externally;
5. reactivates the journal scheme only when it is still the active scheme;
6. verifies terminal outcomes before deleting the journal and its backup.

Crashes and repeated calls are safe. A malformed journal is preserved as a
`.corrupt` sibling and blocks all new overrides; do not delete or edit it while
the underlying system state is unknown.

## Deterministic support/uninstall command

```powershell
.\claudometer.exe --recover-vibecode
```

This command initializes only settings and the recovery controller. It does not
create application windows, read provider credentials, contact the network,
register alerts, or start updater/provider workers. A nonzero process result
means recovery is still unresolved and uninstall/update must stop.

If a legacy `settings.json.vibecode_lid` pair exists, invoking this command is
an explicit request to apply those saved AC/DC values to the *current* scheme.
The operation is journaled first and the legacy pair is cleared only after
read-back verification. The Settings “Restore” action performs the same flow
and explains the current-scheme limitation.

## Manual escalation

If recovery remains blocked, retain all of these files for local inspection:

- `power-override.v1.json`
- `power-override.v1.json.bak`
- every `power-override.v1.json.corrupt.*` sibling
- `settings.json` and its verified backup

Do not guess a scheme, force an old scheme active, clear `vibecode_lid`, or
delete recovery files merely to unblock installation. Resolve the exact power
state first; the uninstaller must abort while the journal cannot converge.
