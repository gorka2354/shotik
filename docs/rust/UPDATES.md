# Updates

An installed copy looks for a newer release once a day and offers it. One click downloads it, checks its signature,
installs it and restarts. Nothing happens without that click: a silent update could restart the application in the middle
of a recording. "Check for updates" in the settings turns the daily look off; "Check now" works either way.

The look is two small static files from GitHub. It sends nothing about the user beyond what any download does: an IP
address and that this is a request for these files.

## What is trusted, and why

```
github.com/gorka2354/shotik/releases/latest/download/latest.json       {"version","sha256","size"}
github.com/gorka2354/shotik/releases/latest/download/latest.json.sig   Ed25519 signature, hex
github.com/gorka2354/shotik/releases/download/v{version}/Shotik-Setup-{version}.exe
```

The signature covers `shotik-update-manifest-v1\n` followed by the exact bytes of `latest.json`. The application checks it
before it even parses the manifest, against the public key built into it (`UPDATE_PUBLIC_KEY` in
`crates/shotik-core/src/update.rs`). The installer must then match the signed size and SHA-256, or it never reaches the disk.
The installer's address is built from the verified version: nothing from the network decides where it comes from.

**The private key is not on GitHub, on purpose.** A signature made by a GitHub Actions secret stops nobody who takes over the
account: they push a workflow and it signs for them. This way, whoever controls the repository can change every file on the
release page and still cannot get code run on a user's machine: CI builds a draft, and only the machine with the key can turn
it into an update. The key is `SHOTIK_UPDATE_KEY` in `.env` at the repository root, which git ignores.

What this does not stop:

- **A malicious commit nobody notices.** CI builds what is in the repository, and a signature vouches for the file that was
  checked, not for how it was built. So read what went into a release before signing it.
- **A stolen key.** Rotate it as below. Copies that have not updated yet trust the old one until they do.
- **Holding copies back.** Whoever controls the release page can keep serving an older signed manifest, or nothing. A copy never
  goes below the version it runs: only a strictly newer version is offered.
- **A copy on a compromised machine.** Anything running as the user can replace the executable without any of this.
- **Downloads by hand.** The signature protects the button. An installer or portable file taken from the release page by hand is
  checked by nothing, and whoever controls the page can replace those.
- **SmartScreen and antivirus.** The installer is not code-signed. A file the application downloads itself carries no mark of the
  web, so SmartScreen does not ask; an antivirus may still block it, and then the application says so and keeps running.

## Publishing

1. Raise the version, merge, push the tag. CI builds a draft, without the signed files.
2. Download the installer from the draft, install it, check it. Keep that file.
3. `node installer/publish-release.js v2.1.0 --installer <that file> --notes-file inc/release-notes-2.1.0.md` signs that file and
   nothing else. It refuses if the installer on the draft has become a different file since: whoever controls the repository can
   swap it at any moment, and a signature on their file is the whole attack. Then it attaches the signed files, publishes, and
   checks that `latest.json` is live.

A release published any other way has no `latest.json`, so copies fail their check, say so in the settings and stay where they
are: they keep working and pick up the next properly signed release.

## Key

If the key is lost, nothing breaks, but no copy can be updated any more: each user has to install a new version by hand once, and
that version carries the new key. So keep a copy somewhere other than this disk, such as a password manager.

Rotation, while the old key is still safe:

1. `node installer/sign-update.js --keygen <file>` writes a new key to a file and prints its public half. It refuses to overwrite
   a key.
2. Put the public half in `UPDATE_PUBLIC_KEY`, release that version **signed with the old key**. The check in
   `sign-update.js` compares against the source of the tree it runs from, so run it from the commit before the change.
3. Replace `SHOTIK_UPDATE_KEY` in `.env`. Later releases are signed with the new key.

## The installer's side

The application runs `Setup.exe /S /RESTART [/SHOWMAIN] /D=<the folder it runs from>` and quits.

- `/D=` comes last and takes no quotes, even with spaces: this is NSIS's rule, so it goes in as one raw argument.
- The new executable is written next to the old one and moved over it in one call (`MoveFileEx`), so neither a full disk nor an
  install cut off halfway leaves no program. If the new one then fails to start, nothing brings the old one back: it is gone.
- It gives the application a moment to leave on its own, then stops whatever still runs from that exact folder.
- When it is done it starts the application with `--updated`; if it could not replace the executable, with `--update-failed`, and
  exit code 2. `--hidden` unless `/SHOWMAIN` was given, which the application passes when its main window was open.
- It leaves a desktop shortcut the user deleted deleted.

A copy with no `Uninstall Shotik.exe` next to it is portable: an installer would add a second copy elsewhere, so the button opens
the release page instead. The installer sits in its own new `%TEMP%\shotik-update-*` folder, because it loads libraries from
where it sits; the next version removes that folder a minute after it starts.

## Not interrupting

This is the one toast nobody asked for. While Windows says the user should not be disturbed (a fullscreen game or presentation, a
locked screen: `SHQueryUserNotificationState`), it waits, and the hourly look tries again. The tray and the main window carry the
offer meanwhile. With notifications turned off in the settings it never appears at all.

## Not installing now

The button refuses while recording, while a recording is still being written after its safety timeout, or with the capture overlay
open. It looks before downloading and again after, since a recording may have started meanwhile. From that last look until the
process leaves, new recordings and overlays are refused, so nothing can start in the gap and be cut off. The offer stays.

Checking is quiet in the background: no network is no reason for a toast. A failure goes to the log and to the settings row, and
is retried within the hour. A check the user asked for always gets an answer.

## Testing

- `crates/shotik-core/src/update.rs`: signature, manifest, versions.
- `crates/shotik-app/src/tests.rs`: the whole flow on fake ports.
- `test/ghost/update.test.js`: the real binary over real WinHTTP, against a local server.
- `test/unit/sign-update.test.js`: the release script. It pins the same bytes as `a_signature_from_the_release_script_verifies` on
  the Rust side, so a format change in either one fails a test instead of cutting off every installed copy.
- `installer/update-smoke.js <Setup.exe>`: the real installer, replacing a real running copy in a private folder. It also runs in
  CI. On a machine with a real install, give it one built with `/DNO_SHELL_ENTRIES`, which writes no shortcuts or registry entries.

`SHOTIK_UPDATE_BASE` and `SHOTIK_UPDATE_PUBLIC_KEY` point a test instance at a test server. Only a test instance reads them,
because they decide whom to trust. Without them a test instance never checks by itself.
