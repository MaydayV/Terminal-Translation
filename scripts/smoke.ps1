$ErrorActionPreference = "Stop"

Write-Host "[smoke] running core tests"
cargo test -p tetr-core

Write-Host "[smoke] building tetr-cli"
cargo build -p tetr-cli

Write-Host "[smoke] install UI deps"
pnpm --dir apps/tetr-ui install

Write-Host "[smoke] build UI"
pnpm --dir apps/tetr-ui build

Write-Host ""
Write-Host "Manual run:"
Write-Host "  $env:TETR_PROVIDER='mock'; cargo run -p tetr-cli -- --no-ui"
