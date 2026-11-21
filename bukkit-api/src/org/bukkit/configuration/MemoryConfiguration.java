package org.bukkit.configuration;

public class MemoryConfiguration extends MemorySection implements Configuration {
    protected Configuration defaults;
    public MemoryConfiguration() {}
    @Override public void addDefault(String path, Object value) { if (defaults == null) defaults = new MemoryConfiguration(); defaults.set(path, value); }
    @Override public void setDefaults(Configuration d) { defaults = d; }
    @Override public Configuration getDefaults() { return defaults; }
}
