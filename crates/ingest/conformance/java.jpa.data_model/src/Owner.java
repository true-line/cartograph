package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.JoinColumn;
import jakarta.persistence.OneToMany;
import jakarta.persistence.Table;
import java.util.List;

@Entity
@Table(name = "owners")
public class Owner {
    @Id
    private Integer id;

    @OneToMany
    @JoinColumn(name = "owner_id")
    private List<Pet> pets;
}
