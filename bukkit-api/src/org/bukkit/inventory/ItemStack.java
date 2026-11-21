package org.bukkit.inventory;

import org.bukkit.Material;

public class ItemStack implements Cloneable {
    private Material type;
    private int amount;
    public ItemStack(Material type) { this(type, 1); }
    public ItemStack(Material type, int amount) { this.type = type; this.amount = amount; }
    public Material getType() { return type; }
    public void setType(Material t) { type = t; }
    public int getAmount() { return amount; }
    public void setAmount(int a) { amount = a; }
    @Override public ItemStack clone() { try { return (ItemStack) super.clone(); } catch (CloneNotSupportedException e) { throw new Error(e); } }
    @Override public String toString() { return "ItemStack{" + type + " x " + amount + "}"; }
}
