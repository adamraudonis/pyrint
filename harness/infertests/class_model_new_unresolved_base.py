import copy
from nonexistent import AbstractOperator


class BaseOperatorMeta(type):
    pass


class BaseOperator(AbstractOperator, metaclass=BaseOperatorMeta):
    def __deepcopy__(self, memo):
        cls = self.__class__
        result = cls.__new__(cls)
        memo[id(self)] = result
        for k, v_org in self.__dict__.items():
            v = copy.deepcopy(v_org, memo)
            result.__dict__[k] = v
        return result
