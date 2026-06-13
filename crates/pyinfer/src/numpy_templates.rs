//! GENERATED from astroid 4.0.4 brain_numpy_* sources (see harness notes).
//! (name, template_source) pairs for the numpy inference tips.

/// brain_numpy_core_function_base.METHODS_TO_BE_INFERRED
pub const NUMPY_FUNCTION_BASE_SRC: [(&str, &str); 3] = [
    ("linspace", r#"def linspace(start, stop, num=50, endpoint=True, retstep=False, dtype=None, axis=0):
            return numpy.ndarray([0, 0])"#),
    ("logspace", r#"def logspace(start, stop, num=50, endpoint=True, base=10.0, dtype=None, axis=0):
            return numpy.ndarray([0, 0])"#),
    ("geomspace", r#"def geomspace(start, stop, num=50, endpoint=True, dtype=None, axis=0):
            return numpy.ndarray([0, 0])"#),
];

/// brain_numpy_core_multiarray.METHODS_TO_BE_INFERRED
pub const NUMPY_MULTIARRAY_SRC: [(&str, &str); 20] = [
    ("array", r#"def array(object, dtype=None, copy=True, order='K', subok=False, ndmin=0):
            return numpy.ndarray([0, 0])"#),
    ("dot", r#"def dot(a, b, out=None):
            return numpy.ndarray([0, 0])"#),
    ("empty_like", r#"def empty_like(a, dtype=None, order='K', subok=True):
            return numpy.ndarray((0, 0))"#),
    ("concatenate", r#"def concatenate(arrays, axis=None, out=None):
            return numpy.ndarray((0, 0))"#),
    ("where", r#"def where(condition, x=None, y=None):
            return numpy.ndarray([0, 0])"#),
    ("empty", r#"def empty(shape, dtype=float, order='C'):
            return numpy.ndarray([0, 0])"#),
    ("bincount", r#"def bincount(x, weights=None, minlength=0):
            return numpy.ndarray([0, 0])"#),
    ("busday_count", r#"def busday_count(
        begindates, enddates, weekmask='1111100', holidays=[], busdaycal=None, out=None
    ):
        return numpy.ndarray([0, 0])"#),
    ("busday_offset", r#"def busday_offset(
        dates, offsets, roll='raise', weekmask='1111100', holidays=None,
        busdaycal=None, out=None
    ):
        return numpy.ndarray([0, 0])"#),
    ("can_cast", r#"def can_cast(from_, to, casting='safe'):
            return True"#),
    ("copyto", r#"def copyto(dst, src, casting='same_kind', where=True):
            return None"#),
    ("datetime_as_string", r#"def datetime_as_string(arr, unit=None, timezone='naive', casting='same_kind'):
            return numpy.ndarray([0, 0])"#),
    ("is_busday", r#"def is_busday(dates, weekmask='1111100', holidays=None, busdaycal=None, out=None):
            return numpy.ndarray([0, 0])"#),
    ("lexsort", r#"def lexsort(keys, axis=-1):
            return numpy.ndarray([0, 0])"#),
    ("may_share_memory", r#"def may_share_memory(a, b, max_work=None):
            return True"#),
    ("packbits", r#"def packbits(a, axis=None, bitorder='big'):
            return numpy.ndarray([0, 0])"#),
    ("shares_memory", r#"def shares_memory(a, b, max_work=None):
            return True"#),
    ("unpackbits", r#"def unpackbits(a, axis=None, count=None, bitorder='big'):
            return numpy.ndarray([0, 0])"#),
    ("unravel_index", r#"def unravel_index(indices, shape, order='C'):
            return (numpy.ndarray([0, 0]),)"#),
    ("zeros", r#"def zeros(shape, dtype=float, order='C'):
            return numpy.ndarray([0, 0])"#),
];

/// brain_numpy_core_numeric.METHODS_TO_BE_INFERRED
pub const NUMPY_NUMERIC_SRC: [(&str, &str); 1] = [
    ("ones", r#"def ones(shape, dtype=None, order='C'):
            return numpy.ndarray([0, 0])"#),
];

/// brain_numpy_ndarray template (numpy_supports_type_hints() is False
/// in the pinned venv — numpy not importable — so no __class_getitem__).
pub const NUMPY_NDARRAY_SRC: &str = r#"
class ndarray(object):
    def __init__(self, shape, dtype=float, buffer=None, offset=0,
                 strides=None, order=None):
        self.T = numpy.ndarray([0, 0])
        self.base = None
        self.ctypes = None
        self.data = None
        self.dtype = None
        self.flags = None
        # Should be a numpy.flatiter instance but not available for now
        # Putting an array instead so that iteration and indexing are authorized
        self.flat = np.ndarray([0, 0])
        self.imag = np.ndarray([0, 0])
        self.itemsize = None
        self.nbytes = None
        self.ndim = None
        self.real = np.ndarray([0, 0])
        self.shape = numpy.ndarray([0, 0])
        self.size = None
        self.strides = None

    def __abs__(self): return numpy.ndarray([0, 0])
    def __add__(self, value): return numpy.ndarray([0, 0])
    def __and__(self, value): return numpy.ndarray([0, 0])
    def __array__(self, dtype=None): return numpy.ndarray([0, 0])
    def __array_wrap__(self, obj): return numpy.ndarray([0, 0])
    def __contains__(self, key): return True
    def __copy__(self): return numpy.ndarray([0, 0])
    def __deepcopy__(self, memo): return numpy.ndarray([0, 0])
    def __divmod__(self, value): return (numpy.ndarray([0, 0]), numpy.ndarray([0, 0]))
    def __eq__(self, value): return numpy.ndarray([0, 0])
    def __float__(self): return 0.
    def __floordiv__(self): return numpy.ndarray([0, 0])
    def __ge__(self, value): return numpy.ndarray([0, 0])
    def __getitem__(self, key): return uninferable
    def __gt__(self, value): return numpy.ndarray([0, 0])
    def __iadd__(self, value): return numpy.ndarray([0, 0])
    def __iand__(self, value): return numpy.ndarray([0, 0])
    def __ifloordiv__(self, value): return numpy.ndarray([0, 0])
    def __ilshift__(self, value): return numpy.ndarray([0, 0])
    def __imod__(self, value): return numpy.ndarray([0, 0])
    def __imul__(self, value): return numpy.ndarray([0, 0])
    def __int__(self): return 0
    def __invert__(self): return numpy.ndarray([0, 0])
    def __ior__(self, value): return numpy.ndarray([0, 0])
    def __ipow__(self, value): return numpy.ndarray([0, 0])
    def __irshift__(self, value): return numpy.ndarray([0, 0])
    def __isub__(self, value): return numpy.ndarray([0, 0])
    def __itruediv__(self, value): return numpy.ndarray([0, 0])
    def __ixor__(self, value): return numpy.ndarray([0, 0])
    def __le__(self, value): return numpy.ndarray([0, 0])
    def __len__(self): return 1
    def __lshift__(self, value): return numpy.ndarray([0, 0])
    def __lt__(self, value): return numpy.ndarray([0, 0])
    def __matmul__(self, value): return numpy.ndarray([0, 0])
    def __mod__(self, value): return numpy.ndarray([0, 0])
    def __mul__(self, value): return numpy.ndarray([0, 0])
    def __ne__(self, value): return numpy.ndarray([0, 0])
    def __neg__(self): return numpy.ndarray([0, 0])
    def __or__(self, value): return numpy.ndarray([0, 0])
    def __pos__(self): return numpy.ndarray([0, 0])
    def __pow__(self): return numpy.ndarray([0, 0])
    def __repr__(self): return str()
    def __rshift__(self): return numpy.ndarray([0, 0])
    def __setitem__(self, key, value): return uninferable
    def __str__(self): return str()
    def __sub__(self, value): return numpy.ndarray([0, 0])
    def __truediv__(self, value): return numpy.ndarray([0, 0])
    def __xor__(self, value): return numpy.ndarray([0, 0])
    def all(self, axis=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def any(self, axis=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def argmax(self, axis=None, out=None): return np.ndarray([0, 0])
    def argmin(self, axis=None, out=None): return np.ndarray([0, 0])
    def argpartition(self, kth, axis=-1, kind='introselect', order=None): return np.ndarray([0, 0])
    def argsort(self, axis=-1, kind='quicksort', order=None): return np.ndarray([0, 0])
    def astype(self, dtype, order='K', casting='unsafe', subok=True, copy=True): return np.ndarray([0, 0])
    def byteswap(self, inplace=False): return np.ndarray([0, 0])
    def choose(self, choices, out=None, mode='raise'): return np.ndarray([0, 0])
    def clip(self, min=None, max=None, out=None): return np.ndarray([0, 0])
    def compress(self, condition, axis=None, out=None): return np.ndarray([0, 0])
    def conj(self): return np.ndarray([0, 0])
    def conjugate(self): return np.ndarray([0, 0])
    def copy(self, order='C'): return np.ndarray([0, 0])
    def cumprod(self, axis=None, dtype=None, out=None): return np.ndarray([0, 0])
    def cumsum(self, axis=None, dtype=None, out=None): return np.ndarray([0, 0])
    def diagonal(self, offset=0, axis1=0, axis2=1): return np.ndarray([0, 0])
    def dot(self, b, out=None): return np.ndarray([0, 0])
    def dump(self, file): return None
    def dumps(self): return str()
    def fill(self, value): return None
    def flatten(self, order='C'): return np.ndarray([0, 0])
    def getfield(self, dtype, offset=0): return np.ndarray([0, 0])
    def item(self, *args): return uninferable
    def itemset(self, *args): return None
    def max(self, axis=None, out=None): return np.ndarray([0, 0])
    def mean(self, axis=None, dtype=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def min(self, axis=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def newbyteorder(self, new_order='S'): return np.ndarray([0, 0])
    def nonzero(self): return (1,)
    def partition(self, kth, axis=-1, kind='introselect', order=None): return None
    def prod(self, axis=None, dtype=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def ptp(self, axis=None, out=None): return np.ndarray([0, 0])
    def put(self, indices, values, mode='raise'): return None
    def ravel(self, order='C'): return np.ndarray([0, 0])
    def repeat(self, repeats, axis=None): return np.ndarray([0, 0])
    def reshape(self, shape, order='C'): return np.ndarray([0, 0])
    def resize(self, new_shape, refcheck=True): return None
    def round(self, decimals=0, out=None): return np.ndarray([0, 0])
    def searchsorted(self, v, side='left', sorter=None): return np.ndarray([0, 0])
    def setfield(self, val, dtype, offset=0): return None
    def setflags(self, write=None, align=None, uic=None): return None
    def sort(self, axis=-1, kind='quicksort', order=None): return None
    def squeeze(self, axis=None): return np.ndarray([0, 0])
    def std(self, axis=None, dtype=None, out=None, ddof=0, keepdims=False): return np.ndarray([0, 0])
    def sum(self, axis=None, dtype=None, out=None, keepdims=False): return np.ndarray([0, 0])
    def swapaxes(self, axis1, axis2): return np.ndarray([0, 0])
    def take(self, indices, axis=None, out=None, mode='raise'): return np.ndarray([0, 0])
    def tobytes(self, order='C'): return b''
    def tofile(self, fid, sep="", format="%s"): return None
    def tolist(self, ): return []
    def tostring(self, order='C'): return b''
    def trace(self, offset=0, axis1=0, axis2=1, dtype=None, out=None): return np.ndarray([0, 0])
    def transpose(self, *axes): return np.ndarray([0, 0])
    def var(self, axis=None, dtype=None, out=None, ddof=0, keepdims=False): return np.ndarray([0, 0])
    def view(self, dtype=None, type=None): return np.ndarray([0, 0])
"#;
