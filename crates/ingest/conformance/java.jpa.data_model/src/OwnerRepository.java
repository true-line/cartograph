package com.demo;

import org.springframework.data.repository.Repository;

public interface OwnerRepository extends Repository<Owner, Integer> {
    Owner findById(Integer id);
    Owner save(Owner owner);
}
