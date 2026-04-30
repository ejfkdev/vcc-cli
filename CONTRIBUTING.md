# Contributing to VCC

Thanks for your interest in contributing! Here's how to get started.

## Development Setup

```bash
git clone https://github.com/ejfkdev/vcc-cli.git
cd vcc-cli
cargo build
cargo test
```

## Making Changes

1. Fork the repository and create a branch from `main`
2. Make your changes with clear, focused commits
3. Ensure `cargo fmt --check` and `cargo clippy` pass
4. Add tests for new functionality
5. Submit a pull request

## Adding a New Tool Adapter

VCC uses config-driven adapters defined in TOML files under `src/config/adapter_mappings/`. To add support for a new tool:

1. Create a new TOML file named after the tool (e.g., `mytool.toml`)
2. Define the tool's config directory, session format, and field mappings
3. Add the `include_str!` entry in `src/config/mod.rs::adapter_mapping_content()`
4. Add the tool name to the `tools.adapters` list in `src/config/resource_registry.toml`
5. If the tool uses a new session format, add the parsing logic in `src/session/`

## Reporting Issues

- Open a [GitHub Issue](https://github.com/ejfkdev/vcc-cli/issues)
- Include the tool version (`vcc --version`), OS, and steps to reproduce

## License

By contributing, you agree that your contributions will be licensed under the MIT License.
