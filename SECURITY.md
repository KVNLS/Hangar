# Security

Hangar runs coding agents on your machine, gives them a local MCP server (a Unix socket under the app's
data directory, one secret per workspace) and drives simulators and emulators. Bugs in those paths can
matter, so please report them privately.

## Reporting a vulnerability

Use GitHub's private vulnerability reporting: **Security › Report a vulnerability** on
[KVNLS/Hangar](https://github.com/KVNLS/Hangar/security/advisories/new). Please don't open a public issue.

Include what you found, how to reproduce it and the version (`Hangar.app` › About, or the git commit).
You should get an answer within a week. Fixes ship in the next release, with credit unless you'd rather not.

## Supported versions

Only the latest release and `main` get fixes.
