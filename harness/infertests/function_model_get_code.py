# pylint: disable=missing-docstring,wrong-import-position,unnecessary-dunder-call

# +1: [too-many-arguments, too-many-positional-arguments]
def stupid_function(arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9):
    return arg1, arg2, arg3, arg4, arg5, arg6, arg7, arg8, arg9


class MyClass:
    text = "MyText"

    def mymethod1(self):
        return self.text

    def mymethod2(self):
        return self.mymethod1.__get__(self, MyClass)


MyClass().mymethod2()()



class WrapperClass:
    def method(self):
        var = +4294967296
        self.method.__code__.co_consts
