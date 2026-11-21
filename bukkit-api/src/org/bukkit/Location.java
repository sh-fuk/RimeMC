package org.bukkit;

import org.bukkit.block.Block;

public class Location implements Cloneable {
    private World world;
    private double x, y, z;
    private float yaw, pitch;

    public Location(World world, double x, double y, double z) { this(world, x, y, z, 0f, 0f); }
    public Location(World world, double x, double y, double z, float yaw, float pitch) { this.world = world; this.x = x; this.y = y; this.z = z; this.yaw = yaw; this.pitch = pitch; }

    public World getWorld() { return world; }
    public void setWorld(World w) { world = w; }
    public double getX() { return x; }
    public double getY() { return y; }
    public double getZ() { return z; }
    public void setX(double v) { x = v; }
    public void setY(double v) { y = v; }
    public void setZ(double v) { z = v; }
    public float getYaw() { return yaw; }
    public float getPitch() { return pitch; }
    public void setYaw(float v) { yaw = v; }
    public void setPitch(float v) { pitch = v; }
    public int getBlockX() { return (int) Math.floor(x); }
    public int getBlockY() { return (int) Math.floor(y); }
    public int getBlockZ() { return (int) Math.floor(z); }
    public Block getBlock() { return world.getBlockAt(this); }
    public Location add(double dx, double dy, double dz) { x += dx; y += dy; z += dz; return this; }
    public Location subtract(double dx, double dy, double dz) { x -= dx; y -= dy; z -= dz; return this; }
    public double distanceSquared(Location o) { double a = x - o.x, b = y - o.y, c = z - o.z; return a * a + b * b + c * c; }
    public double distance(Location o) { return Math.sqrt(distanceSquared(o)); }
    @Override public Location clone() { try { return (Location) super.clone(); } catch (CloneNotSupportedException e) { throw new Error(e); } }
    @Override public String toString() { return "Location{world=" + (world == null ? null : world.getName()) + ",x=" + x + ",y=" + y + ",z=" + z + ",pitch=" + pitch + ",yaw=" + yaw + "}"; }
}
