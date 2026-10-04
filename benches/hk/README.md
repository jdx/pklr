hk's Pkl configuration, vendored from [jdx/hk](https://github.com/jdx/hk) at
v1.39.0 (MIT, see `LICENSE`) so the `hk` benchmark in `tak.toml` evaluates a
real-world config without the network.

- `hk.pkl` is hk's own `hk.pkl` at that tag.
- `pkl/` is hk's `pkl/` directory. `pkl/Builtins.pkl` is generated in hk by
  `scripts/gen_builtins.py`; `PklProject` files are left out.

Replacing these files changes what the benchmark measures, so update them in a
commit of their own.
