package org.bukkit;

public final class NamespacedKey {
    public static final String MINECRAFT = "minecraft";
    private final String namespace, key;
    public NamespacedKey(String namespace, String key) { this.namespace = namespace; this.key = key; }
    public static NamespacedKey minecraft(String key) { return new NamespacedKey(MINECRAFT, key); }
    public String getNamespace() { return namespace; }
    public String getKey() { return key; }
    @Override public String toString() { return namespace + ":" + key; }
    @Override public boolean equals(Object o) { return o instanceof NamespacedKey && o.toString().equals(toString()); }
    @Override public int hashCode() { return toString().hashCode(); }
}
