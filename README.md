To find unused dependencies in production target:
```
cargo +nightly udeps
```

To find unused dev dependencies:
```
cargo +nightly udeps --all-targets
```