package com.demo;

public class OwnerService {
    private final OwnerRepository owners;

    public OwnerService(OwnerRepository owners) {
        this.owners = owners;
    }

    public Owner rename(Integer id) {
        return owners.save(owners.findById(id));
    }
}
