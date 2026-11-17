package demo;

class Config {
    static String getenv(String key) {
        return key;
    }

    String[] load(String name) {
        return new String[] { System.getenv("DATABASE_URL"), System.getenv(name) };
    }

    String lookalike() {
        return getenv("SHADOWED");
    }
}
