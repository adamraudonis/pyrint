from os import PathLike

FilePath = str | "PathLike[str]"
x = FilePath
y = str | PathLike[str]
z = y
