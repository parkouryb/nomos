# Claude Desktop / Claude Code Rules - Nomos Resource Safety

## MANDATORY: Nomos Resource Arbiter Enforcement
This host is strictly regulated by Nomos Resource Arbiter to prevent CPU starvation, memory exhaustion (OOM), and desktop UI freezes.

### Execution Policy:
1. **MCP Tools Preferred**:
   - Always prefer using the native Nomos MCP tools: `nomos_run`, `nomos_acquire`, `nomos_release`, `nomos_probe`, `nomos_status`, and `nomos_accounting`.
2. **Terminal / Bash Fallback**:
   - When executing commands via Bash, wrap medium and heavy commands inside Nomos leases:
     ```bash
     nomos run --cpu <cores> --mem <ram> --ttl <seconds> -- <command>
     ```
   - Quick metadata queries (`ls`, `pwd`, `git status`) may be run directly or will be auto-assigned lightweight leases.
3. **Web Telemetry**:
   - Inspect live resource allocation and 30-day accounting ledger at `http://localhost:9100`.
