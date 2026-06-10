x = 1.5
precision = 2
y = f"{x: .{precision:d}f}"
z = f"{x:{precision}.{precision:>{precision}d}f}"
