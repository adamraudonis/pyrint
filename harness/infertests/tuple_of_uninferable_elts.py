def f(details):
    proxy_details = [
        details.get("username"),
        details.get("password"),
    ]
    if "verify_ssl" in details:
        proxy_details.append(details.get("verify_ssl"))
    return tuple(proxy_details)


def caller(d):
    ret = f(d)
    ret
