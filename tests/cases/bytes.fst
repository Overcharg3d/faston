# bytes and bytearray
let b = b"hello\x00\xffworld";
print(b, len(b), b[0], b[-1], b[6]);
print(b[1:4], b[::-1][:3], type(b), b"" == bytes(), bool(b""));
print(b"ab" + b"cd", b"ab" * 3, b"ell" in b, 104 in b, 7 in b);
print(b"abc" < b"abd", b"abc" == bytearray(b"abc"), b"a" == "a");
print(list(b"AZ"), [x * 2 for x in b"\x01\x02"]);
print(bytes(3), bytes([65, 66, 67]), bytes("héllo", "utf-8"), bytes(range(3)));
print(rb"a\nb", br"\x00", B"x", repr(b"it's"), repr(b'say "hi"'));
print(b"\t\n\r\\" , b"\x7f\x80");

# methods
let s = b"  The quick brown fox  ";
print(s.strip(), s.lstrip(), s.rstrip(), b"xxhixx".strip(b"x"));
print(s.split(), b"a,b,,c".split(b","), b"a b c".split(None, 1));
print(b"-".join([b"a", b"b", b"c"]), b"aaa".replace(b"a", b"bb"), b"aaa".replace(b"a", b"b", 2));
print(b"hello".find(b"l"), b"hello".rfind(b"l"), b"hello".find(b"z"), b"hello".index(b"lo"));
print(b"hello".count(b"l"), b"hello".count(108), b"hello".startswith(b"he"), b"hello".endswith((b"x", b"lo")));
print(b"Hello".upper(), b"Hello".lower(), b"123".isdigit(), b"abc".isalpha(), b" \t".isspace());
print(b"\xde\xad\xbe\xef".hex(), bytes.fromhex("de ad be ef"), bytearray.fromhex("0001"));

# text
print("héllo €".encode(), "abc".encode("ascii"), "café".encode("latin-1"));
print(b"h\xc3\xa9llo".decode(), b"abc".decode("ascii"), b"caf\xe9".decode("latin-1"));
print(b"bad \xff byte".decode("utf-8", "replace"), b"bad \xff byte".decode(errors="ignore"));
print("é".encode("ascii", "replace"), "é".encode("ascii", errors="ignore"));
try { b"\xff".decode(); } except (UnicodeDecodeError as e) { print("UnicodeDecodeError:", e); }
try { "é".encode("ascii"); } except (UnicodeEncodeError as e) { print("UnicodeEncodeError:", e); }

# bytearray
let ba = bytearray(b"abc");
ba.append(100);
ba.extend(b"ef");
ba.extend([103, 104]);
ba[0] = 65;
print(ba, len(ba), ba.pop(), ba.pop(0), ba);
ba.insert(1, 120);
ba.remove(120);
ba.reverse();
print(ba, bytes(ba), ba.copy() == ba, ba + b"!", type(ba + b"!"));
let ba2 = bytearray(2);
ba2[1] = 255;
print(ba2, ba2.hex());
try { ba2[0] = 256; } except (ValueError as e) { print("ValueError:", e); }
try { let x = b"abc"; x[0] = 1; } except (TypeError as e) { print("TypeError:", e); }

# ints
print((1024).to_bytes(2, "big"), (1024).to_bytes(2, "little"), (255).to_bytes(), (-2).to_bytes(2, "big", signed=True));
print(int.from_bytes(b"\x04\x00", "big"), int.from_bytes(b"\x04\x00", "little"), int.from_bytes(b"\xff\xfe", "big", signed=True));
print(int.from_bytes([1, 0]), (300).bit_length(), (0).bit_length());
try { (256).to_bytes(1, "big"); } except (OverflowError as e) { print("OverflowError:", e); }

# hashing, sets, dict keys, isinstance
let d = {b"k": 1};
print(d[b"k"], b"k" in d, len({b"a", b"a", b"b"}));
print(isinstance(b"", bytes), isinstance(bytearray(), bytearray), isinstance(b"", bytearray), isinstance(5, int), isinstance(True, int), isinstance("s", (int, str)));
try { let h = {bytearray(b"x"): 1}; } except (TypeError as e) { print("TypeError:", e); }
print("\x41é\U0001F600", "\a\b\f\v" == "\x07\x08\x0c\x0b");
