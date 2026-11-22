---
"cartograph": minor
---

TypeScript calls on a class that inherits the method it calls, such as `map.get()` on a `DisposableResourceMap extends DisposableMap`, now link to the base class method instead of staying an open gap. Methods of abstract classes are now named after their class (`Disposable.dispose`) rather than anonymously (#258, AC-1514).
