files=(*.ox)

for src in "${files[@]}"; do
    cargo run --quiet --bin oxide -- \
        "$src" \
        --emit ir \
        -o "${src%.ox}.ll" \
        "$@"
done
