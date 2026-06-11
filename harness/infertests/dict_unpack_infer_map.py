def to_latex(escape, decimal):
    base_format_ = {
        "na_rep": "NaN",
        "escape": "latex" if escape else None,
        "decimal": decimal,
    }
    index_format_ = {"axis": 0, **base_format_}
    plain_ = {"axis": 1, "x": 2}
    index_format_.update({"formatter": None})
    print(index_format_)
    print(plain_)
    fi = [index_format_, plain_]
    print(fi)
