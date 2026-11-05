# raw strings and triple-quoted strings (expected output generated with CPython)
print(r"C:\new\table", len(r"\n"), r'a\'b', r"\d+\s*");
let block = """first "line"
second 'line'\tend""";
print(block);
print('''it's "fine"''', len("""
x
"""));
let name = "Ann";
print(f"""Hi {name},
total: {1 + 2}""");
print(r"""raw \n triple""", "" + "x", """""" == "");
