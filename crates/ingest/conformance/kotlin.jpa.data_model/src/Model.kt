package demo

import jakarta.persistence.Entity
import jakarta.persistence.Id
import jakarta.persistence.ManyToOne
import jakarta.persistence.Table

@Entity
@Table(name = "owners")
class Owner(@Id val id: Long, val name: String)

@Entity
class Pet(
    @Id val id: Long,
    @ManyToOne val owner: Owner,
)
