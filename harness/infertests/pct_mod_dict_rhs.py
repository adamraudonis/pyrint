obj = {}
msg = "value '%s'." % obj
msg
one = "a=%(a)s" % {"a": 1}
one
two = "%s %s" % {"a": 1}
two
three = "%d" % {}
three
four = "%s" % {1: 2}
four
five = "%s" % {"a": [1,2]}
five
six = "%(b)s" % {"a": 1}
six
seven = "%s" % ()
seven
eight = "%s %s" % (1,)
eight
nine = "%10s|" % {"a": 1}
nine
ten = "x" % "y"
ten
eleven = "x" % {}
eleven
twelve = "%(a)s %s" % {"a": 1}
twelve
thirteen = "%s" % {"a": 1, "a": 2, 1: "x", True: "y"}
thirteen
fourteen = "%.4s" % {"a": 1}
fourteen
fifteen = "%s %(a)s" % {"a": 1}
fifteen
sixteen = "%(a)s %(a)s" % {"a": 1}
sixteen
