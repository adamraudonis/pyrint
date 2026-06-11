def two():
    if cond:
        return 1
    return 2

def i18n_patterns(*urls):
    if not flag:
        return list(urls)
    return [urls]

urlpatterns = i18n_patterns(
    two(),
    two(),
)
ok = i18n_patterns("a", "b")
