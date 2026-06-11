SUFFIX = "--username airflow"

TEST_COMMANDS = [
    f"auth token {SUFFIX}",
    "assets list {date_param}",
    "xcom get {xcom_key}",
]

DONE = [t.format(date_param="d", xcom_key="k") for t in TEST_COMMANDS]


def g():
    name = str(len(DONE))
    return name.format()
