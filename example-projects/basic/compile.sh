files=(*.ox)

for src in "${files[@]}"; do
    cargo run --quiet --example oxide-codegen-example -- \
        -f "$src" \
        -o "${src%.ox}.ll" \
        "$@"
done
