package org.bukkit.configuration;

public interface Configuration extends ConfigurationSection {
    void addDefault(String path, Object value);
    void setDefaults(Configuration defaults);
    Configuration getDefaults();
}
