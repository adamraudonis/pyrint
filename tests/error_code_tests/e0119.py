# Test file for error code E0119: misplaced-format-function
# This file contains code that triggers E0119 (format function is not called on str)

# Example 1: format called on int
x = 123
result1 = x.format("test")  # E0119: misplaced-format-function

# Example 2: format called on list
my_list = [1, 2, 3]
result2 = my_list.format("test")  # E0119: misplaced-format-function

# Example 3: format called on dict
my_dict = {"key": "value"}
result3 = my_dict.format("test")  # E0119: misplaced-format-function

# Example 4: format called on None
value = None
result4 = value.format("test")  # E0119: misplaced-format-function

# Example 5: format called on bool
flag = True
result5 = flag.format("test")  # E0119: misplaced-format-function

# Valid cases - format on strings
string1 = "Hello {}"
result_ok1 = string1.format("World")  # OK

string2 = "Value: {value}"
result_ok2 = string2.format(value=42)  # OK

# Format on string literal
result_ok3 = "Test {}".format("value")  # OK