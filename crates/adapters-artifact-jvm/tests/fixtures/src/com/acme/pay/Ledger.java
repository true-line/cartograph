package com.acme.pay;

import java.util.List;
import java.util.function.Function;
import java.util.function.Supplier;

public class Ledger {
    private final Store store = new Store();

    public Ledger() {}

    public long total(List<String> ids, int scale) {
        long sum = 0;
        for (String id : ids) {
            sum += store.amount(id) * scale;
        }
        return sum;
    }

    public static Ledger open() {
        return new Ledger();
    }

    public void post(Audit audit, String id) {
        audit.record(id);
        Supplier<Long> later = () -> store.amount(id);
        later.get();
        Function<String, Long> named = store::amount;
        named.apply(id);
        switch (id.length()) {
            case 1: case 2: case 3: audit.record("short"); break;
            default: break;
        }
        switch (id.hashCode()) {
            case 10: case 2000: case 300000: audit.record("hashed"); break;
            default: break;
        }
    }

    public void accepts(java.time.Clock clock) {}

    class Entry {
        final int amount;

        Entry(int amount) {
            this.amount = amount;
        }
    }

    enum Kind {
        DEBIT(-1),
        CREDIT(1);

        final int sign;

        Kind(int sign) {
            this.sign = sign;
        }
    }

    static class Store {
        long amount(String id) {
            return id.length();
        }
    }

    interface Audit {
        void record(String line);
    }
}
