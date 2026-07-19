# Security Policy

Odometer processes local AI-agent session files that contain sensitive content (prompts, replies, tool output, filesystem paths). Anything that could exfiltrate, corrupt, or over-expose that data is in scope, as is anything affecting the update mechanism's signature verification.

## Supported versions

Only the [latest release](https://github.com/ekalb81/agent-odometer/releases/latest) is supported; the app self-updates.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting: **Security → Report a vulnerability** on this repository. Don't open public issues for security problems. You should get an initial response within a week.
