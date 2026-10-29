# C example

Builds against the generated header and the engine cdylib:

```sh
cargo build -p quercus-engine
cc examples/c/hello.c \
  -I crates/quercus-engine/include \
  -L target/debug -lquercus_engine \
  -Wl,-rpath,"$PWD/target/debug" \
  -o /tmp/quercus-hello
/tmp/quercus-hello
```

On Windows use the import library in `target/debug` and copy
`quercus_engine.dll` next to the executable.
