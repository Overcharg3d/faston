# datetime: date, time, datetime, timedelta (naive: no time zones).
# The calendar math follows CPython's datetime.py.
import _datetime;

MINYEAR = 1;
MAXYEAR = 9999;

_DIM = [-1, 31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
_DBM = [-1, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];

def _is_leap(y) { return y % 4 == 0 and (y % 100 != 0 or y % 400 == 0); }

def _days_in_month(y, m) {
    if (m == 2 and _is_leap(y)) { return 29; }
    return _DIM[m];
}

def _days_before_year(y) {
    let y1 = y - 1;
    return y1 * 365 + y1 // 4 - y1 // 100 + y1 // 400;
}

def _days_before_month(y, m) { return _DBM[m] + (1 if m > 2 and _is_leap(y) else 0); }

def _ymd2ord(y, m, d) { return _days_before_year(y) + _days_before_month(y, m) + d; }

_DI400Y = _days_before_year(401);
_DI100Y = _days_before_year(101);
_DI4Y = _days_before_year(5);

def _ord2ymd(n) {
    n -= 1;
    n400, n = divmod(n, _DI400Y);
    let year = n400 * 400 + 1;
    n100, n = divmod(n, _DI100Y);
    n4, n = divmod(n, _DI4Y);
    n1, n = divmod(n, 365);
    year += n100 * 100 + n4 * 4 + n1;
    if (n1 == 4 or n100 == 4) { return (year - 1, 12, 31); }
    let leapyear = n1 == 3 and (n4 != 24 or n100 == 3);
    let month = (n + 50) >> 5;
    let preceding = _DBM[month] + (1 if month > 2 and leapyear else 0);
    if (preceding > n) {
        month -= 1;
        preceding -= _DIM[month] + (1 if month == 2 and leapyear else 0);
    }
    return (year, month, n - preceding + 1);
}

def _check_date(year, month, day) {
    if (not (MINYEAR <= year <= MAXYEAR)) { raise ValueError(f"year {year} is out of range"); }
    if (not (1 <= month <= 12)) { raise ValueError("month must be in 1..12"); }
    if (not (1 <= day <= _days_in_month(year, month))) { raise ValueError("day is out of range for month"); }
}

def _check_time(hour, minute, second, microsecond) {
    if (not (0 <= hour <= 23)) { raise ValueError("hour must be in 0..23"); }
    if (not (0 <= minute <= 59)) { raise ValueError("minute must be in 0..59"); }
    if (not (0 <= second <= 59)) { raise ValueError("second must be in 0..59"); }
    if (not (0 <= microsecond <= 999999)) { raise ValueError("microsecond must be in 0..999999"); }
}

def _time_text(h, m, s, us) {
    let out = f"{h:02d}:{m:02d}:{s:02d}";
    if (us) { out += f".{us:06d}"; }
    return out;
}

def _parse_time(text, whole) {
    let parts = text.split(":");
    if (len(parts) < 2 or len(parts) > 3) { raise ValueError(f"Invalid isoformat string: {repr(whole)}"); }
    let h = int(parts[0]);
    let m = int(parts[1]);
    let s = 0;
    let us = 0;
    if (len(parts) == 3) {
        let sec = parts[2].split(".");
        s = int(sec[0]);
        if (len(sec) == 2) { us = int((sec[1] + "000000")[:6]); }
    }
    return (h, m, s, us);
}

class timedelta {
    def init(self, days = 0, seconds = 0, microseconds = 0, milliseconds = 0, minutes = 0, hours = 0, weeks = 0) {
        let us = ((weeks * 7 + days) * 86400 + hours * 3600 + minutes * 60 + seconds) * 1000000 + milliseconds * 1000 + microseconds;
        self._set(round(us));
    }

    def _set(self, us) {
        self.days = us // 86400000000;
        let rest = us % 86400000000;
        self.seconds = rest // 1000000;
        self.microseconds = rest % 1000000;
    }

    def _us(self) { return (self.days * 86400 + self.seconds) * 1000000 + self.microseconds; }
    def total_seconds(self) { return self._us() / 1000000; }

    def __add__(self, other) {
        if (isinstance(other, timedelta)) { return _td(self._us() + other._us()); }
        if (isinstance(other, date)) { return other + self; }
        raise TypeError("unsupported operand type(s) for +: 'timedelta' and '" + type(other) + "'");
    }
    def __radd__(self, other) { return self.__add__(other); }
    def __sub__(self, other) {
        if (isinstance(other, timedelta)) { return _td(self._us() - other._us()); }
        raise TypeError("unsupported operand type(s) for -: 'timedelta' and '" + type(other) + "'");
    }
    def __neg__(self) { return _td(-self._us()); }
    def __mul__(self, n) { return _td(round(self._us() * n)); }
    def __rmul__(self, n) { return _td(round(self._us() * n)); }
    def __truediv__(self, other) {
        if (isinstance(other, timedelta)) { return self._us() / other._us(); }
        return _td(round(self._us() / other));
    }
    def __floordiv__(self, other) {
        if (isinstance(other, timedelta)) { return self._us() // other._us(); }
        return _td(self._us() // other);
    }
    def __mod__(self, other) { return _td(self._us() % other._us()); }
    def __eq__(self, other) { return isinstance(other, timedelta) and self._us() == other._us(); }
    def __ne__(self, other) { return not self.__eq__(other); }
    def __lt__(self, other) { return self._us() < other._us(); }
    def __le__(self, other) { return self._us() <= other._us(); }
    def __gt__(self, other) { return self._us() > other._us(); }
    def __ge__(self, other) { return self._us() >= other._us(); }
    def __bool__(self) { return self._us() != 0; }

    def __str__(self) {
        let s = self.seconds;
        let out = _time_text(s // 3600, s % 3600 // 60, s % 60, self.microseconds);
        if (out.startswith("0") and len(out) > 1 and out[1] != ":") { out = out[1:]; }
        if (self.days) {
            out = f"{self.days} day" + ("s" if abs(self.days) != 1 else "") + ", " + out;
        }
        return out;
    }

    def __repr__(self) {
        let parts = [];
        if (self.days) { parts.append(f"days={self.days}"); }
        if (self.seconds) { parts.append(f"seconds={self.seconds}"); }
        if (self.microseconds) { parts.append(f"microseconds={self.microseconds}"); }
        if (not parts) { parts.append("0"); }
        return "datetime.timedelta(" + ", ".join(parts) + ")";
    }
}

def _td(us) {
    let t = timedelta();
    t._set(us);
    return t;
}

class date {
    def init(self, year, month, day) {
        _check_date(year, month, day);
        self.year = year;
        self.month = month;
        self.day = day;
    }

    @classmethod
    def today(cls) {
        let p = _datetime.now();
        return cls(p[0], p[1], p[2]);
    }
    @classmethod
    def fromordinal(cls, n) {
        let ymd = _ord2ymd(n);
        return cls(ymd[0], ymd[1], ymd[2]);
    }
    @classmethod
    def fromtimestamp(cls, ts) {
        let p = _datetime.from_timestamp(ts, False);
        return cls(p[0], p[1], p[2]);
    }
    @classmethod
    def fromisoformat(cls, text) {
        let parts = text.split("-");
        if (len(parts) != 3) { raise ValueError(f"Invalid isoformat string: {repr(text)}"); }
        return cls(int(parts[0]), int(parts[1]), int(parts[2]));
    }

    def toordinal(self) { return _ymd2ord(self.year, self.month, self.day); }
    def weekday(self) { return (self.toordinal() + 6) % 7; }
    def isoweekday(self) { return self.toordinal() % 7 or 7; }
    def isoformat(self) { return f"{self.year:04d}-{self.month:02d}-{self.day:02d}"; }
    def strftime(self, fmt) { return _datetime.strftime(fmt, self.year, self.month, self.day, 0, 0, 0, 0); }
    def replace(self, year = None, month = None, day = None) {
        return date(self.year if year is None else year, self.month if month is None else month, self.day if day is None else day);
    }
    def _key(self) { return (self.year, self.month, self.day); }

    def __add__(self, other) {
        if (isinstance(other, timedelta)) { return date.fromordinal(self.toordinal() + other.days); }
        raise TypeError("unsupported operand type(s) for +: 'date' and '" + type(other) + "'");
    }
    def __radd__(self, other) { return self.__add__(other); }
    def __sub__(self, other) {
        if (isinstance(other, timedelta)) { return self + (-other); }
        if (isinstance(other, date)) { return timedelta(self.toordinal() - other.toordinal()); }
        raise TypeError("unsupported operand type(s) for -: 'date' and '" + type(other) + "'");
    }
    def __eq__(self, other) { return type(other) == type(self) and self._key() == other._key(); }
    def __ne__(self, other) { return not self.__eq__(other); }
    def __lt__(self, other) { return self._key() < other._key(); }
    def __le__(self, other) { return self._key() <= other._key(); }
    def __gt__(self, other) { return self._key() > other._key(); }
    def __ge__(self, other) { return self._key() >= other._key(); }
    def __str__(self) { return self.isoformat(); }
    def __repr__(self) { return f"datetime.date({self.year}, {self.month}, {self.day})"; }
}

class time {
    def init(self, hour = 0, minute = 0, second = 0, microsecond = 0) {
        _check_time(hour, minute, second, microsecond);
        self.hour = hour;
        self.minute = minute;
        self.second = second;
        self.microsecond = microsecond;
    }
    @classmethod
    def fromisoformat(cls, text) {
        let p = _parse_time(text, text);
        return cls(p[0], p[1], p[2], p[3]);
    }
    def isoformat(self) { return _time_text(self.hour, self.minute, self.second, self.microsecond); }
    def strftime(self, fmt) {
        return _datetime.strftime(fmt, 1900, 1, 1, self.hour, self.minute, self.second, self.microsecond);
    }
    def replace(self, hour = None, minute = None, second = None, microsecond = None) {
        return time(
            self.hour if hour is None else hour,
            self.minute if minute is None else minute,
            self.second if second is None else second,
            self.microsecond if microsecond is None else microsecond
        );
    }
    def _key(self) { return (self.hour, self.minute, self.second, self.microsecond); }
    def __eq__(self, other) { return isinstance(other, time) and self._key() == other._key(); }
    def __ne__(self, other) { return not self.__eq__(other); }
    def __lt__(self, other) { return self._key() < other._key(); }
    def __le__(self, other) { return self._key() <= other._key(); }
    def __gt__(self, other) { return self._key() > other._key(); }
    def __ge__(self, other) { return self._key() >= other._key(); }
    def __str__(self) { return self.isoformat(); }
    def __repr__(self) {
        let parts = [self.hour, self.minute];
        if (self.second or self.microsecond) { parts.append(self.second); }
        if (self.microsecond) { parts.append(self.microsecond); }
        return "datetime.time(" + ", ".join([str(x) for x in parts]) + ")";
    }
}

class datetime(date) {
    def init(self, year, month, day, hour = 0, minute = 0, second = 0, microsecond = 0) {
        super().init(year, month, day);
        _check_time(hour, minute, second, microsecond);
        self.hour = hour;
        self.minute = minute;
        self.second = second;
        self.microsecond = microsecond;
    }

    @classmethod
    def now(cls) { return cls(*_datetime.now()); }
    @classmethod
    def today(cls) { return cls(*_datetime.now()); }
    @classmethod
    def utcnow(cls) { return cls(*_datetime.utcnow()); }
    @classmethod
    def fromtimestamp(cls, ts) { return cls(*_datetime.from_timestamp(ts, False)); }
    @classmethod
    def utcfromtimestamp(cls, ts) { return cls(*_datetime.from_timestamp(ts, True)); }
    @classmethod
    def strptime(cls, text, fmt) { return cls(*_datetime.strptime(text, fmt)); }
    @classmethod
    def combine(cls, d, t) { return cls(d.year, d.month, d.day, t.hour, t.minute, t.second, t.microsecond); }
    @classmethod
    def fromisoformat(cls, text) {
        let sep = "T" if "T" in text else " ";
        let halves = text.split(sep, 1);
        let d = date.fromisoformat(halves[0]);
        if (len(halves) == 1) { return cls(d.year, d.month, d.day); }
        let t = _parse_time(halves[1], text);
        return cls(d.year, d.month, d.day, t[0], t[1], t[2], t[3]);
    }

    def timestamp(self) {
        return _datetime.to_timestamp(self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond);
    }
    def date(self) { return date(self.year, self.month, self.day); }
    def time(self) { return time(self.hour, self.minute, self.second, self.microsecond); }
    def isoformat(self, sep = "T") {
        return f"{self.year:04d}-{self.month:02d}-{self.day:02d}" + sep + _time_text(self.hour, self.minute, self.second, self.microsecond);
    }
    def strftime(self, fmt) {
        return _datetime.strftime(fmt, self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond);
    }
    def replace(self, year = None, month = None, day = None, hour = None, minute = None, second = None, microsecond = None) {
        return datetime(
            self.year if year is None else year,
            self.month if month is None else month,
            self.day if day is None else day,
            self.hour if hour is None else hour,
            self.minute if minute is None else minute,
            self.second if second is None else second,
            self.microsecond if microsecond is None else microsecond
        );
    }
    def _key(self) { return (self.year, self.month, self.day, self.hour, self.minute, self.second, self.microsecond); }
    def _abs_us(self) {
        return (self.toordinal() * 86400 + self.hour * 3600 + self.minute * 60 + self.second) * 1000000 + self.microsecond;
    }

    def __add__(self, other) {
        if (isinstance(other, timedelta)) { return _dt_from_us(self._abs_us() + other._us()); }
        raise TypeError("unsupported operand type(s) for +: 'datetime' and '" + type(other) + "'");
    }
    def __radd__(self, other) { return self.__add__(other); }
    def __sub__(self, other) {
        if (isinstance(other, timedelta)) { return _dt_from_us(self._abs_us() - other._us()); }
        if (isinstance(other, datetime)) { return _td(self._abs_us() - other._abs_us()); }
        raise TypeError("unsupported operand type(s) for -: 'datetime' and '" + type(other) + "'");
    }
    def __str__(self) { return self.isoformat(" "); }
    def __repr__(self) {
        let parts = [self.year, self.month, self.day, self.hour, self.minute];
        if (self.second or self.microsecond) { parts.append(self.second); }
        if (self.microsecond) { parts.append(self.microsecond); }
        return "datetime.datetime(" + ", ".join([str(x) for x in parts]) + ")";
    }
}

def _dt_from_us(us) {
    days, rest = divmod(us, 86400000000);
    let ymd = _ord2ymd(days);
    h, rest = divmod(rest, 3600000000);
    m, rest = divmod(rest, 60000000);
    s, micro = divmod(rest, 1000000);
    return datetime(ymd[0], ymd[1], ymd[2], h, m, s, micro);
}

timedelta.resolution = timedelta(microseconds=1);
