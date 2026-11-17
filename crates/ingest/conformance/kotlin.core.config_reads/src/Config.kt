package demo

fun getenv(key: String): String = key

fun load(name: String): List<String?> =
    listOf(System.getenv("DATABASE_URL"), System.getenv(name))

fun lookalike(): String = getenv("SHADOWED")
