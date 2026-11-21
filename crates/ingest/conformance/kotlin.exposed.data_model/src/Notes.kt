package demo

import org.jetbrains.exposed.sql.Table
import org.jetbrains.exposed.sql.insert
import org.jetbrains.exposed.sql.selectAll

object Notes : Table("notes") {
    val id = integer("id")
    val body = text("body")
}

class NoteStore {
    fun all() = Notes.selectAll().toList()
    fun add(text: String) = Notes.insert { it[body] = text }
}
