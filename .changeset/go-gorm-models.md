---
"cartograph": patch
---

Go repositories now have their GORM models recovered (#2019): each model is a data entity with its table and columns, and the functions that create, read, update and delete it through GORM read and write it, across packages. GORM models also become domain candidates. On the Gin RealWorld example this recovers 7 models and 28 reads and writes, with 2 gaps where a model arrives as an untyped `interface{}`.
