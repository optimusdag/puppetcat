# Security policy

## Supported versions

Only the latest release is supported. PuppetCat is pre-release software, so
fixes land on `main` and ship in the next release.

## Reporting a vulnerability

Please report it privately, not in a public issue: open the repository's
**Security** tab, choose **Report a vulnerability**, and describe what you
found and how to reproduce it.

You can expect an acknowledgement within a few days. Once a fix is available
the report can be disclosed publicly, and you will be credited if you wish.

PuppetCat runs entirely on your machine and exposes an MCP server over stdio,
plus a local socket that lets the MCP server reach a running window. Reports
about either, about how projects and media files are parsed, and about the
build and release process are all in scope.

## Conduct reports

Reports under the [Code of Conduct](CODE_OF_CONDUCT.md) can use the same
private form; start the title with "Conduct:".
