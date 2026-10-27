# Platforms development

Keep device interfaces and portable runtime contracts in Core; implement platform backends here. Preserve package names, runtime persistence and device permissions. Use `mise run test:seed`, `mise run build:native`, and `mise run check:browser` as applicable. Seed generation consumes separately built application artifacts through `XILA_APPLICATIONS_DIR`; do not assume applications are workspace members. Package checksum verification is not publisher authentication. Do not claim hardware, browser or Docker validation without running it on that target.
