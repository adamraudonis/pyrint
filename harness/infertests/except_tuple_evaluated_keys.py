class NoBluetoothAdapter(Exception):
    pass


class NoDevicesFound(Exception):
    pass


EXCEPTION_MAP = {NoBluetoothAdapter: "no_adapter", NoDevicesFound: "none"}


def f():
    try:
        pass
    except tuple(EXCEPTION_MAP.keys()) as e:
        errors = {"base": EXCEPTION_MAP.get(type(e), str(type(e)))}
        return errors
