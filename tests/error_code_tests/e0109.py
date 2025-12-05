# Test file for error code E0109: duplicate-key
# This file contains code that triggers E0109 (Duplicate key in dictionary)

# Example 1: Simple duplicate string keys
dict1 = {
    "key1": 1,
    "key2": 2,
    "key1": 3,  # E0109: duplicate-key
}

# Example 2: Duplicate integer keys
dict2 = {
    1: "one",
    2: "two",
    1: "one again",  # E0109: duplicate-key
}

# Example 3: Multiple duplicates
dict3 = {
    "a": 1,
    "b": 2,
    "a": 3,  # E0109: duplicate-key
    "c": 4,
    "b": 5,  # E0109: duplicate-key
}

# Example 4: Mixed types but same string representation
dict4 = {
    True: 1,
    False: 2,
    True: 3,  # E0109: duplicate-key
}

# Example 5: None as duplicate key
dict5 = {
    None: "none1",
    "key": "value",
    None: "none2",  # E0109: duplicate-key
}

# Example 6: Nested dictionary with duplicates
dict6 = {
    "outer1": {"inner": 1},
    "outer2": {
        "x": 1,
        "y": 2,
        "x": 3,  # E0109: duplicate-key
    },
    "outer1": {"inner": 2},  # E0109: duplicate-key
}

# Valid case: Dynamic keys (not checked)
key_name = "dynamic"
dict7 = {
    key_name: 1,
    "static": 2,
    # Cannot detect duplicate with dynamic keys
}