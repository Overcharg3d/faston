class BaseException {
    def init(self, message = "") { self.message = message; }
    def __str__(self) { return str(self.message); }
    def __repr__(self) { return type(self) + "(" + repr(self.message) + ")"; }
}
class Exception(BaseException) { pass; }
class ArithmeticError(Exception) { pass; }
class ZeroDivisionError(ArithmeticError) { pass; }
class OverflowError(ArithmeticError) { pass; }
class LookupError(Exception) { pass; }
class IndexError(LookupError) { pass; }
class KeyError(LookupError) { pass; }
class TypeError(Exception) { pass; }
class ValueError(Exception) { pass; }
class NameError(Exception) { pass; }
class AttributeError(Exception) { pass; }
class RuntimeError(Exception) { pass; }
class RecursionError(RuntimeError) { pass; }
class NotImplementedError(RuntimeError) { pass; }
class AssertionError(Exception) { pass; }
class ImportError(Exception) { pass; }
class OSError(Exception) { pass; }
class IOError(OSError) { pass; }
class FileNotFoundError(OSError) { pass; }
class FileExistsError(OSError) { pass; }
class PermissionError(OSError) { pass; }
class IsADirectoryError(OSError) { pass; }
class NotADirectoryError(OSError) { pass; }
class TimeoutError(OSError) { pass; }
class EOFError(Exception) { pass; }
class ModuleNotFoundError(ImportError) { pass; }
class UnicodeError(ValueError) { pass; }
class UnicodeDecodeError(UnicodeError) { pass; }
class UnicodeEncodeError(UnicodeError) { pass; }
class Enum {
    def __str__(self) { return type(self) + "." + self.name; }
    def __repr__(self) { return "<" + type(self) + "." + self.name + ": " + repr(self.value) + ">"; }
}
class JSONDecodeError(ValueError) { pass; }
class SyntaxError(Exception) { pass; }
class StopIteration(Exception) { pass; }
class KeyboardInterrupt(BaseException) { pass; }
class GeneratorExit(BaseException) { pass; }
class property {
    def init(self, fget = None, fset = None, fdel = None, doc = None) {
        self.fget = fget; self.fset = fset; self.fdel = fdel; self.__doc__ = doc;
    }
    def getter(self, f) { return property(f, self.fset, self.fdel, self.__doc__); }
    def setter(self, f) { return property(self.fget, f, self.fdel, self.__doc__); }
    def deleter(self, f) { return property(self.fget, self.fset, f, self.__doc__); }
}
class staticmethod {
    def init(self, f) { self.__func__ = f; }
    def __call__(self, *args, **kwargs) { return self.__func__(*args, **kwargs); }
}
class classmethod {
    def init(self, f) { self.__func__ = f; }
}
