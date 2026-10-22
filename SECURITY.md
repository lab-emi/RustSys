# Security policy

RustSys reads design files that may come from untrusted sources. Parsers are expected to reject
malformed input with an error and never panic, hang or exhaust memory without bound.

To report a vulnerability, contact the maintainers privately (see the repository profile) and
include a minimal input that triggers the problem. Please do not open a public issue for
security reports. We aim to acknowledge reports within seven days.
