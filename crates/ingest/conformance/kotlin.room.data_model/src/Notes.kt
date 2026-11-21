package demo

import androidx.room.Dao
import androidx.room.Entity
import androidx.room.Insert
import androidx.room.PrimaryKey
import androidx.room.Query

@Entity(tableName = "notes")
data class Note(@PrimaryKey val id: Long, val body: String)

@Dao
interface NoteDao {
    @Query("SELECT * FROM notes")
    fun all(): List<Note>

    @Insert
    fun add(note: Note)
}
