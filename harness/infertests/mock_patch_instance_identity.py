from unittest import mock

POD = "pod.manager.Class"


def use():
    with mock.patch(f"{POD}.await_pod_completion") as a:
        a.return_value = 1


def use2():
    with mock.patch("pod.manager.x") as b:
        b.return_value = 2


def use3():
    with mock.patch(POD) as c:
        c.return_value = 3
