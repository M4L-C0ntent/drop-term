prefix := env_var_or_default("PREFIX", env_var("HOME") / ".local")

build:
    cargo build --release

install: build
    cargo install --path . --root {{prefix}} --force
    install -Dm644 data/io.github.m4l_c0ntent.drop-term.desktop {{prefix}}/share/applications/com.github.drop-term.desktop

uninstall:
    cargo uninstall --root {{prefix}} drop-term
    rm -f {{prefix}}/share/applications/io.github.m4l_c0ntent.drop-term.desktop
