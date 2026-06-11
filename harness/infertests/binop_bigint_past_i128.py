IPV6LENGTH = 128
ALL_ONES = (2 ** IPV6LENGTH) - 1
x = ALL_ONES
y = int('f' * 32, 16)
big = 2 ** 128
shifted = big >> 1
