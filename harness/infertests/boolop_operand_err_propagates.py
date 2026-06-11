import codecs
import locale


def get_system_encoding():
    try:
        encoding = locale.getlocale()[1] or "ascii"
        codecs.lookup(encoding)
    except Exception:
        encoding = "ascii"
    return encoding


def f():
    e = get_system_encoding()
    return e
