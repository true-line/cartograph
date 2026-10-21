package com.demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;
import jakarta.persistence.ManyToOne;

@Entity
public class Pet {
    @Id
    private Integer id;

    @ManyToOne
    private Owner owner;
}
