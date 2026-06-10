import os
import datetime
LAST_JID_DATETIME = None
def _utc_now():
    return datetime.datetime.utcnow()
def gen():
    global LAST_JID_DATETIME
    jid_dt = _utc_now()
    if LAST_JID_DATETIME and LAST_JID_DATETIME >= jid_dt:
        jid_dt = LAST_JID_DATETIME + datetime.timedelta(microseconds=1)
    LAST_JID_DATETIME = jid_dt
    x = f"{jid_dt:%Y%m%d%H%M%S%f}_{os.getpid()}"
    y = f"{jid_dt:%Y%m%d%H%M%S%f}"
    return x
r = gen()
r
