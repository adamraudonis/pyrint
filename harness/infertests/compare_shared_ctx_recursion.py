import numpy as np


class RangeIndex:
    pass


class MultiIndex:
    @property
    def _codes(self):
        return [1]


class Index:
    @property
    def _values(self):
        return object()


class Series(Index):
    pass


class NDArrayBackedExtensionArray:
    _ndarray = None


class SparseArray:
    sp_values = None


class IntervalArray:
    _left = None
    _right = None


class ArrowExtensionArray:
    _pa_array = None


class BaseMaskedArray:
    _data = None
    _mask = None


class DataFrame:
    _mgr = None


def shares_memory(left, right) -> bool:
    if isinstance(left, np.ndarray) and isinstance(right, np.ndarray):
        return np.shares_memory(left, right)
    elif isinstance(left, np.ndarray):
        return shares_memory(right, left)

    if isinstance(left, RangeIndex):
        return False
    if isinstance(left, MultiIndex):
        return shares_memory(left._codes, right)
    if isinstance(left, (Index, Series)):
        if isinstance(right, (Index, Series)):
            return shares_memory(left._values, right._values)
        return shares_memory(left._values, right)

    if isinstance(left, NDArrayBackedExtensionArray):
        return shares_memory(left._ndarray, right)
    if isinstance(left, SparseArray):
        return shares_memory(left.sp_values, right)
    if isinstance(left, IntervalArray):
        return shares_memory(left._left, right) or shares_memory(left._right, right)

    if isinstance(left, ArrowExtensionArray):
        if isinstance(right, ArrowExtensionArray):
            left_pa_data = left._pa_array
            right_pa_data = right._pa_array
            left_buf1 = left_pa_data.chunk(0).buffers()[1]
            right_buf1 = right_pa_data.chunk(0).buffers()[1]
            return left_buf1.address == right_buf1.address
        else:
            return np.shares_memory(left, right)

    if isinstance(left, BaseMaskedArray) and isinstance(right, BaseMaskedArray):
        return np.shares_memory(left._data, right._data) or np.shares_memory(
            left._mask, right._mask
        )

    if isinstance(left, DataFrame) and len(left._mgr.blocks) == 1:
        arr = left._mgr.blocks[0].values
        return shares_memory(arr, right)

    raise NotImplementedError(type(left), type(right))


def test(obj):
    result = obj.infer_objects()
    assert shares_memory(result, obj)
