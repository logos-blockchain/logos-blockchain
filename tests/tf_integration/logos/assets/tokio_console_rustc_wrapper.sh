#!/bin/sh
# Cargo passes its compiler (or workspace wrapper) as the first argument.
has_cfg=false
previous=
for arg do
    if [ "$arg" = '--cfg=tokio_unstable' ] || { [ "$previous" = '--cfg' ] && [ "$arg" = 'tokio_unstable' ]; }; then
        has_cfg=true
    fi
    previous=$arg
done
if [ "$has_cfg" = false ]; then
    set -- "$@" --cfg tokio_unstable
fi
if [ -n "$LOGOS_ORIGINAL_RUSTC_WRAPPER" ]; then
    exec "$LOGOS_ORIGINAL_RUSTC_WRAPPER" "$@"
fi
exec "$@"
