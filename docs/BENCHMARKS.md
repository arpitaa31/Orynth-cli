# Benchmarks

Status: canonical specification derived from the supplied research blueprint.

Measure startup, idle RSS, one-agent execution, mostly waiting agents, event append/reconstruction, context projection, prompt rendering, and provider-stream overhead. Separate the harness from local model and plugin processes.

Long-term desirable targets are under 25 MB CLI idle, under 35 MB TUI idle, under 50 MB for one remote agent, under 75 MB for four agents, and under 100 MB for ten mostly waiting agents. These are measurements to earn, not current claims. The first harness follows runtime and persistence.

