import datetime
future = (datetime.date.today() + datetime.timedelta(days=60)).replace(day=1)
future
urlbit = future.strftime("%Y/%b").lower()
